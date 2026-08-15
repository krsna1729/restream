// Raw-UDP tier of the TCP vs UDP vs SRT scaling ladder: no ARQ, no
// encryption, no TSBPD, no congestion control -- isolates pure kernel/NIC/
// CPU cost of moving N x 8Mbps of datagrams from whatever SRT's protocol
// stack adds on top.
//
// Per-thread ownership (see sender_bench.c for the full rationale): each
// worker thread touches only its own private connection state, never a
// shared array filtered by owner.
//
// Timer wheel, not a per-tick linear scan: a plain "scan all owned
// connections, check next_due" loop is O(N) every tick -- fine at low N,
// but at 1200 connections on one thread the scan itself dominates over
// actual sends (measured: ~9% of target throughput, worse than multi-
// threaded scanning). All streams here share nearly the same pacing
// interval, so a fixed-size ring of time slots (a classic single-level
// timer wheel) gives O(1) amortized "what's due now" with no kernel timer
// involved (no timerfd) and no per-connection heap/tree: one wheel
// rotation is sized to equal one pacing interval, so a connection lands
// back in roughly the same slot every lap, and advancing time is just
// walking to the next slot index.
//
// Usage: udp_sender <host> <port_base> <port_count> <threads> <bitrate_Bps>
//                    <c1,c2,...> <hold_secs> [local_port_count] [local_port_base] [cpu_base]
//
// cpu_base (default 1): first CPU core this sender's worker threads pin to
// (thread i -> core cpu_base+i). Must not overlap the receiver's core
// range in a separate process, and core 0 is worth avoiding on most hosts
// -- it tends to absorb interrupt/softirq/kernel housekeeping noise that a
// pinned benchmark thread would otherwise contend with.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <unistd.h>
#include <time.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <errno.h>
#include <x86intrin.h>

#define PAYLOAD_SIZE 1316
#define WHEEL_SLOTS 64

static void pin_to_cpu(int cpu) {
    int nproc = sysconf(_SC_NPROCESSORS_ONLN);
    if (nproc <= 0) return;
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu % nproc, &set);
    pthread_setaffinity_np(pthread_self(), sizeof(set), &set);
}

// rdtscp instead of clock_gettime() in the hot loop: a serializing TSC read
// (a handful of cycles, no vDSO call) vs. clock_gettime()'s ~20-30ns even
// on the fast vDSO path -- matters at the spin frequency this loop runs
// at. Requires an invariant TSC (true on any x86_64 host since roughly
// Nehalem) and calibration against CLOCK_MONOTONIC once at startup, done
// on a pinned thread so the calibration itself isn't skewed by migration.
static uint64_t g_tsc_hz;

static inline uint64_t rdtsc_now(void) {
    unsigned int aux;
    return __rdtscp(&aux);
}

static uint64_t calibrate_tsc_hz(void) {
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    uint64_t tsc0 = rdtsc_now();
    usleep(50000);
    clock_gettime(CLOCK_MONOTONIC, &t1);
    uint64_t tsc1 = rdtsc_now();
    double elapsed_s = (t1.tv_sec - t0.tv_sec) + (t1.tv_nsec - t0.tv_nsec) / 1e9;
    return (uint64_t)((double)(tsc1 - tsc0) / elapsed_s);
}

// Owned exclusively by one worker thread once published. No next_due here
// -- wheel slot membership carries the timing, not a per-connection
// timestamp.
typedef struct {
    int fd;
} owned_conn_t;

typedef struct {
    int thread_id;
    owned_conn_t *owned;   // fd storage, indexed by owned-slot id
    int *slot_next;        // intrusive singly-linked list per wheel slot, indexed by owned-slot id, -1 = end
    int slot_head[WHEEL_SLOTS];
    int cur_slot;
    uint64_t slot_duration_ticks;
    uint64_t next_slot_tick;
    int last_seen_n;       // worker-local, no synchronization needed (only this thread reads it)
    _Atomic int n_owned;   // single-producer (main thread) / single-consumer (this thread)
    _Atomic long long bytes_sent;
    _Atomic long long send_attempts;
    _Atomic long long send_errors;
    _Atomic long long send_would_block;
} worker_ctx_t;

static int *g_owner_thread;
static int g_cpu_base = 1;
static double g_interval_ms;
static _Atomic int g_running = 1;

static double ts_diff_ms(const struct timespec *a, const struct timespec *b) {
    return (b->tv_sec - a->tv_sec) * 1000.0 + (b->tv_nsec - a->tv_nsec) / 1e6;
}

static void *worker_thread(void *arg) {
    worker_ctx_t *ctx = (worker_ctx_t *)arg;
    pin_to_cpu(g_cpu_base + ctx->thread_id);
    char *payload = malloc(PAYLOAD_SIZE);
    memset(payload, 0x42, PAYLOAD_SIZE);

    for (int s = 0; s < WHEEL_SLOTS; s++) ctx->slot_head[s] = -1;
    ctx->cur_slot = 0;
    ctx->last_seen_n = 0;
    ctx->next_slot_tick = rdtsc_now();

    while (g_running) {
        // Adopt newly published connections into the current slot -- they
        // fire on the very next slot advance, then join the normal
        // rotation from there.
        int n = atomic_load_explicit(&ctx->n_owned, memory_order_acquire);
        for (int i = ctx->last_seen_n; i < n; i++) {
            ctx->slot_next[i] = ctx->slot_head[ctx->cur_slot];
            ctx->slot_head[ctx->cur_slot] = i;
        }
        ctx->last_seen_n = n;

        uint64_t now = rdtsc_now();
        while (now >= ctx->next_slot_tick) {
            int idx = ctx->slot_head[ctx->cur_slot];
            ctx->slot_head[ctx->cur_slot] = -1;
            while (idx != -1) {
                int next = ctx->slot_next[idx];
                owned_conn_t *c = &ctx->owned[idx];
                atomic_fetch_add(&ctx->send_attempts, 1);
                ssize_t r = send(c->fd, payload, PAYLOAD_SIZE, MSG_DONTWAIT);
                if (r > 0) {
                    atomic_fetch_add(&ctx->bytes_sent, r);
                } else if (r < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
                    atomic_fetch_add(&ctx->send_would_block, 1);
                } else {
                    atomic_fetch_add(&ctx->send_errors, 1);
                }
                // Requeue into the slot just vacated, for the next lap
                // (one full wheel rotation ~= one pacing interval later).
                ctx->slot_next[idx] = ctx->slot_head[ctx->cur_slot];
                ctx->slot_head[ctx->cur_slot] = idx;
                idx = next;
            }
            ctx->cur_slot = (ctx->cur_slot + 1) % WHEEL_SLOTS;
            ctx->next_slot_tick += ctx->slot_duration_ticks;
        }
    }
    free(payload);
    return NULL;
}

int main(int argc, char **argv) {
    if (argc < 8) {
        fprintf(stderr, "usage: %s <host> <port_base> <port_count> <threads> <bitrate_Bps> <c1,c2,...> <hold_secs> [local_port_count] [local_port_base]\n", argv[0]);
        return 1;
    }
    const char *host = argv[1];
    int port_base = atoi(argv[2]);
    int port_count = atoi(argv[3]);
    int nthreads = atoi(argv[4]);
    long bitrate_bps = atol(argv[5]);
    int hold_secs = atoi(argv[7]);
    int local_port_count = (argc >= 9) ? atoi(argv[8]) : 0;
    int local_port_base = (argc >= 10) ? atoi(argv[9]) : 52000;
    g_cpu_base = (argc >= 11) ? atoi(argv[10]) : 1;
    g_interval_ms = (double)PAYLOAD_SIZE * 1000.0 / (double)bitrate_bps;
    g_tsc_hz = calibrate_tsc_hz();
    uint64_t slot_duration_ticks = (uint64_t)(g_interval_ms * 1e-3 / WHEEL_SLOTS * (double)g_tsc_hz);
    fprintf(stderr, "[udp_sender] tsc_hz=%llu slot_duration_ticks=%llu\n",
            (unsigned long long)g_tsc_hz, (unsigned long long)slot_duration_ticks);

    int checkpoints[64];
    int nckpt = 0;
    char *ckpt_str = strdup(argv[6]);
    char *tok = strtok(ckpt_str, ",");
    while (tok && nckpt < 64) {
        checkpoints[nckpt++] = atoi(tok);
        tok = strtok(NULL, ",");
    }
    int max_conns = checkpoints[nckpt - 1];

    g_owner_thread = calloc(max_conns, sizeof(int));
    worker_ctx_t *workers = calloc(nthreads, sizeof(worker_ctx_t));
    pthread_t *threads = calloc(nthreads, sizeof(pthread_t));
    for (int i = 0; i < nthreads; i++) {
        workers[i].thread_id = i;
        workers[i].owned = calloc(max_conns, sizeof(owned_conn_t));
        workers[i].slot_next = calloc(max_conns, sizeof(int));
        workers[i].slot_duration_ticks = slot_duration_ticks;
        atomic_store(&workers[i].n_owned, 0);
        pthread_create(&threads[i], NULL, worker_thread, &workers[i]);
    }

    struct sockaddr_in *addrs = calloc(port_count, sizeof(struct sockaddr_in));
    for (int p = 0; p < port_count; p++) {
        addrs[p].sin_family = AF_INET;
        addrs[p].sin_port = htons(port_base + p);
        inet_pton(AF_INET, host, &addrs[p].sin_addr);
    }

    printf("checkpoint,requested,connected,failed,steady_bytes_sent,steady_send_attempts,steady_send_errors,steady_would_block,target_bytes,pct_of_target,elapsed_connect_s\n");

    int already_started = 0;
    for (int c = 0; c < nckpt; c++) {
        int target = checkpoints[c];
        struct timespec ramp_start, ramp_end;
        clock_gettime(CLOCK_MONOTONIC, &ramp_start);

        int failed_count = 0;
        for (int idx = already_started; idx < target; idx++) {
            int fd = socket(AF_INET, SOCK_DGRAM, 0);
            if (local_port_count > 0) {
                int reuse = 1;
                setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));
                struct sockaddr_in local_addr = {0};
                local_addr.sin_family = AF_INET;
                local_addr.sin_port = htons(local_port_base + (idx % local_port_count));
                local_addr.sin_addr.s_addr = INADDR_ANY;
                bind(fd, (struct sockaddr *)&local_addr, sizeof(local_addr));
            }
            int p = idx % port_count;
            if (connect(fd, (struct sockaddr *)&addrs[p], sizeof(addrs[p])) < 0) {
                close(fd);
                failed_count++;
                continue;
            }
            int owner = idx % nthreads;
            g_owner_thread[idx] = owner;
            worker_ctx_t *w = &workers[owner];
            int slot = atomic_load_explicit(&w->n_owned, memory_order_relaxed);
            w->owned[slot].fd = fd;
            atomic_store_explicit(&w->n_owned, slot + 1, memory_order_release);
        }

        clock_gettime(CLOCK_MONOTONIC, &ramp_end);
        double elapsed_connect_s = ts_diff_ms(&ramp_start, &ramp_end) / 1000.0;

        for (int i = 0; i < nthreads; i++) {
            atomic_store(&workers[i].bytes_sent, 0);
            atomic_store(&workers[i].send_attempts, 0);
            atomic_store(&workers[i].send_errors, 0);
            atomic_store(&workers[i].send_would_block, 0);
        }
        sleep(hold_secs);

        long long total_bytes = 0, total_attempts = 0, total_errs = 0, total_wb = 0;
        for (int i = 0; i < nthreads; i++) {
            total_bytes += atomic_load(&workers[i].bytes_sent);
            total_attempts += atomic_load(&workers[i].send_attempts);
            total_errs += atomic_load(&workers[i].send_errors);
            total_wb += atomic_load(&workers[i].send_would_block);
        }

        int n_new = target - already_started;
        int connected = n_new - failed_count;

        long long target_bytes = (long long)target * bitrate_bps * hold_secs;
        double pct_of_target = target_bytes > 0 ? (100.0 * (double)total_bytes / (double)target_bytes) : 0.0;

        printf("%d,%d,%d,%d,%lld,%lld,%lld,%lld,%lld,%.2f,%.2f\n",
               target, n_new, connected, failed_count,
               total_bytes, total_attempts, total_errs, total_wb, target_bytes, pct_of_target, elapsed_connect_s);
        fflush(stdout);

        already_started = target;
    }

    g_running = 0;
    return 0;
}
