// Companion to sink_bench.c: same 0->N checkpoint ramp as sender.c, but
// spreads new connections round-robin across a port range instead of one
// fixed port, so it can drive sink_bench's port_count>1 configurations.
//
// Steady-state sending uses explicit per-connection deadline pacing, not
// epoll_wait()-on-SRT_EPOLL_OUT opportunistic sends. An earlier
// epoll-driven design (send once per ready-fd per poll) under-called
// srt_sendmsg2() by ~50x at scale -- 0 errors and 0 EASYNCSND, just far
// too few send attempts -- because SRT_EPOLL_OUT readiness for a
// lightly-filled SNDBUF does not reliably re-fire at the cadence a small
// per-message payload needs. Each worker thread instead scans its owned
// slice of the global connection array on a tight timer and sends
// whenever a connection's deadline has passed, guaranteeing the intended
// per-connection call rate regardless of libsrt's internal
// readiness-tracking cadence. Genuine backpressure now shows up honestly
// as send_would_block/send_errors instead of being silently masked as
// "never attempted."
//
// Usage: sender_bench <host> <port_base> <port_count> <threads> <bitrate_Bps>
//                      <c1,c2,...> <hold_secs> [local_port_count] [local_port_base]
//
// local_port_count (default 0 = OS-assigned ephemeral ports, one
// multiplexer's worth of sharing risk left to libsrt's own reuse logic):
// explicitly srt_bind() each outbound socket to one of N local ports
// before connect(), round-robin, mirroring restream's own production
// SrtEgressMuxerPorts sender-side sharding. Needed because libsrt's
// CUDTUnited::updateMux() can consolidate multiple outbound sockets onto
// one shared local multiplexer when they aren't bound to distinct local
// ports -- the same shared-CSndQueue bottleneck shape as the well-known
// listener-side one, just on the send side, and easy to miss testing only
// receive-side port_count in isolation.
#define _GNU_SOURCE
#include <srt/srt.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <unistd.h>
#include <time.h>
#include <arpa/inet.h>
#include <syslog.h>

// Pin the calling thread to one CPU core -- avoids the scheduler migrating
// a hot polling/pacing thread between cores mid-run, which costs cache
// locality and adds jitter that matters at sub-millisecond send cadences.
static void pin_to_cpu(int cpu) {
    int nproc = sysconf(_SC_NPROCESSORS_ONLN);
    if (nproc <= 0) return;
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu % nproc, &set);
    pthread_setaffinity_np(pthread_self(), sizeof(set), &set);
}

#define PAYLOAD_SIZE 1316

typedef struct {
    SRTSOCKET sock;
    int connected;
    int failed;
    int owner_thread;
    struct timespec connect_start;
    double connect_latency_ms;
    struct timespec next_due;
} conn_t;

// Owned exclusively by one worker thread after being published via
// n_owned's release-store -- no other thread ever reads or writes an
// owned_conn_t, so next_due needs no synchronization of its own. This is
// the fix for the earlier design (every thread scanning the full shared
// conn_t[] and filtering by owner_thread): that scanned 6x redundant
// entries per tick and had every thread touching the same cache lines,
// which is real cross-thread contention, not a host CPU ceiling.
typedef struct {
    SRTSOCKET sock;
    struct timespec next_due;
} owned_conn_t;

typedef struct {
    int thread_id;
    owned_conn_t *owned; // private array, single-producer (main thread appends,
                          // release-stores n_owned) / single-consumer (this thread)
    _Atomic int n_owned;
    _Atomic long long bytes_sent;
    _Atomic long long send_attempts;
    _Atomic long long send_errors;
    _Atomic long long send_would_block;
} worker_ctx_t;

static conn_t *g_conns;
static int g_max_conns;
static int g_nthreads;
static double g_interval_ms; // per-connection send interval to hit target bitrate
static _Atomic int g_running = 1;

static double ts_diff_ms(const struct timespec *a, const struct timespec *b) {
    return (b->tv_sec - a->tv_sec) * 1000.0 + (b->tv_nsec - a->tv_nsec) / 1e6;
}

static void ts_add_ms(struct timespec *t, double ms) {
    long ns = t->tv_nsec + (long)(ms * 1e6);
    t->tv_sec += ns / 1000000000L;
    t->tv_nsec = ns % 1000000000L;
}

static void *worker_thread(void *arg) {
    worker_ctx_t *ctx = (worker_ctx_t *)arg;
    pin_to_cpu(ctx->thread_id);
    char *payload = malloc(PAYLOAD_SIZE);
    memset(payload, 0x42, PAYLOAD_SIZE);

    while (g_running) {
        struct timespec now;
        clock_gettime(CLOCK_MONOTONIC, &now);
        int n = atomic_load_explicit(&ctx->n_owned, memory_order_acquire);
        double min_wait_ms = 1.0; // cap: never sleep longer than this, bounds OS scheduling slop
        for (int i = 0; i < n; i++) {
            owned_conn_t *c = &ctx->owned[i];
            double due_in_ms = ts_diff_ms(&now, &c->next_due);
            if (due_in_ms > 0) {
                if (due_in_ms < min_wait_ms) min_wait_ms = due_in_ms;
                continue; // not due yet
            }

            SRT_MSGCTRL mc = srt_msgctrl_default;
            atomic_fetch_add(&ctx->send_attempts, 1);
            int n = srt_sendmsg2(c->sock, payload, PAYLOAD_SIZE, &mc);
            if (n > 0) {
                atomic_fetch_add(&ctx->bytes_sent, n);
                ts_add_ms(&c->next_due, g_interval_ms);
                // If we fell behind (e.g. host contention), resync to now
                // instead of firing a catch-up burst that would misrepresent
                // steady-state achieved rate.
                if (ts_diff_ms(&c->next_due, &now) > 0) {
                    c->next_due = now;
                }
            } else {
                int syserr = 0;
                int err = srt_getlasterror(&syserr);
                if (err == SRT_EASYNCSND) {
                    atomic_fetch_add(&ctx->send_would_block, 1);
                } else {
                    atomic_fetch_add(&ctx->send_errors, 1);
                }
                // Retry ASAP either way -- do not advance next_due.
            }
            min_wait_ms = 0.0; // at least one connection was due this pass; re-scan immediately
        }
        if (min_wait_ms > 0.0) {
            struct timespec sleep_ts;
            sleep_ts.tv_sec = (time_t)(min_wait_ms / 1000.0);
            sleep_ts.tv_nsec = (long)((min_wait_ms - sleep_ts.tv_sec * 1000.0) * 1e6);
            nanosleep(&sleep_ts, NULL);
        }
    }
    free(payload);
    return NULL;
}

int main(int argc, char **argv) {
    if (argc < 8) {
        fprintf(stderr, "usage: %s <host> <port_base> <port_count> <threads> <bitrate_Bps> <c1,c2,...> <hold_secs>\n", argv[0]);
        return 1;
    }
    const char *host = argv[1];
    int port_base = atoi(argv[2]);
    int port_count = atoi(argv[3]);
    int nthreads = atoi(argv[4]);
    long bitrate_bps = atol(argv[5]);
    int hold_secs = atoi(argv[7]);
    int local_port_count = (argc >= 9) ? atoi(argv[8]) : 0;
    int local_port_base = (argc >= 10) ? atoi(argv[9]) : 50000;
    g_nthreads = nthreads;
    g_interval_ms = (double)PAYLOAD_SIZE * 1000.0 / (double)bitrate_bps;

    int checkpoints[64];
    int nckpt = 0;
    char *ckpt_str = strdup(argv[6]);
    char *tok = strtok(ckpt_str, ",");
    while (tok && nckpt < 64) {
        checkpoints[nckpt++] = atoi(tok);
        tok = strtok(NULL, ",");
    }
    int max_conns = checkpoints[nckpt - 1];
    g_max_conns = max_conns;

    srt_startup();
    srt_setloglevel(LOG_CRIT);

    g_conns = calloc(max_conns, sizeof(conn_t));
    worker_ctx_t *workers = calloc(nthreads, sizeof(worker_ctx_t));
    pthread_t *threads = calloc(nthreads, sizeof(pthread_t));
    for (int i = 0; i < nthreads; i++) {
        workers[i].thread_id = i;
        workers[i].owned = calloc(max_conns, sizeof(owned_conn_t)); // worst case: one thread owns all
        atomic_store(&workers[i].n_owned, 0);
        pthread_create(&threads[i], NULL, worker_thread, &workers[i]);
    }

    struct sockaddr_in *addrs = calloc(port_count, sizeof(struct sockaddr_in));
    for (int p = 0; p < port_count; p++) {
        memset(&addrs[p], 0, sizeof(addrs[p]));
        addrs[p].sin_family = AF_INET;
        addrs[p].sin_port = htons(port_base + p);
        inet_pton(AF_INET, host, &addrs[p].sin_addr);
    }

    printf("checkpoint,requested,connected,failed,connect_p50_ms,connect_p95_ms,connect_p99_ms,steady_bytes_sent,steady_send_attempts,steady_send_errors,steady_would_block,target_bytes,pct_of_target,elapsed_connect_s\n");

    int already_started = 0;
    for (int c = 0; c < nckpt; c++) {
        int target = checkpoints[c];
        struct timespec ramp_start, ramp_end;
        clock_gettime(CLOCK_MONOTONIC, &ramp_start);

        for (int idx = already_started; idx < target; idx++) {
            SRTSOCKET s = srt_create_socket();
            int no = 0;
            srt_setsockopt(s, 0, SRTO_RCVSYN, &no, sizeof(no));
            srt_setsockopt(s, 0, SRTO_SNDSYN, &no, sizeof(no));
            int64_t maxbw = bitrate_bps;
            srt_setsockopt(s, 0, SRTO_MAXBW, &maxbw, sizeof(maxbw));
            int latency = 250;
            srt_setsockopt(s, 0, SRTO_LATENCY, &latency, sizeof(latency));
            int sndbuf = 6 * 1024 * 1024;
            srt_setsockopt(s, 0, SRTO_SNDBUF, &sndbuf, sizeof(sndbuf));
            int fc = 32768;
            srt_setsockopt(s, 0, SRTO_FC, &fc, sizeof(fc));

            if (local_port_count > 0) {
                int reuse = 1;
                srt_setsockopt(s, 0, SRTO_REUSEADDR, &reuse, sizeof(reuse));
                struct sockaddr_in local_addr;
                memset(&local_addr, 0, sizeof(local_addr));
                local_addr.sin_family = AF_INET;
                local_addr.sin_port = htons(local_port_base + (idx % local_port_count));
                local_addr.sin_addr.s_addr = INADDR_ANY;
                srt_bind(s, (struct sockaddr *)&local_addr, sizeof(local_addr));
            }

            int p = idx % port_count;
            clock_gettime(CLOCK_MONOTONIC, &g_conns[idx].connect_start);
            int rc = srt_connect(s, (struct sockaddr *)&addrs[p], sizeof(addrs[p]));
            if (rc == SRT_ERROR) {
                g_conns[idx].failed = 1;
                continue;
            }
            g_conns[idx].sock = s;
            g_conns[idx].owner_thread = idx % nthreads;
        }

        int setup_eid = srt_epoll_create();
        for (int idx = already_started; idx < target; idx++) {
            if (g_conns[idx].failed) continue;
            int events = SRT_EPOLL_OUT | SRT_EPOLL_ERR;
            srt_epoll_add_usock(setup_eid, g_conns[idx].sock, &events);
        }
        int remaining = target - already_started;
        for (int i = already_started; i < target; i++) if (g_conns[i].failed) remaining--;
        struct timespec setup_deadline;
        clock_gettime(CLOCK_MONOTONIC, &setup_deadline);
        setup_deadline.tv_sec += 30;
        while (remaining > 0) {
            struct timespec now;
            clock_gettime(CLOCK_MONOTONIC, &now);
            if (now.tv_sec > setup_deadline.tv_sec) break;
            SRTSOCKET wfds[4096];
            int wnum = 4096;
            int r = srt_epoll_wait(setup_eid, NULL, NULL, wfds, &wnum, 200, NULL, NULL, NULL, NULL);
            if (r < 0) continue;
            for (int i = 0; i < wnum; i++) {
                for (int idx = already_started; idx < target; idx++) {
                    if (g_conns[idx].sock == wfds[i] && !g_conns[idx].connected) {
                        struct timespec done;
                        clock_gettime(CLOCK_MONOTONIC, &done);
                        g_conns[idx].connect_latency_ms = ts_diff_ms(&g_conns[idx].connect_start, &done);
                        g_conns[idx].connected = 1;
                        srt_epoll_remove_usock(setup_eid, wfds[i]);
                        remaining--;

                        // Publish into the owning worker's private array --
                        // single producer (this loop), single consumer (that
                        // worker thread); release-store makes sock/next_due
                        // visible before the count bump it's gated on.
                        worker_ctx_t *w = &workers[g_conns[idx].owner_thread];
                        int slot = atomic_load_explicit(&w->n_owned, memory_order_relaxed);
                        w->owned[slot].sock = g_conns[idx].sock;
                        w->owned[slot].next_due = done;
                        atomic_store_explicit(&w->n_owned, slot + 1, memory_order_release);
                    }
                }
            }
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
        double *lat = malloc(sizeof(double) * (n_new > 0 ? n_new : 1));
        int n_connected = 0, n_failed = 0;
        for (int idx = already_started; idx < target; idx++) {
            if (g_conns[idx].connected) lat[n_connected++] = g_conns[idx].connect_latency_ms;
            else n_failed++;
        }
        for (int i = 1; i < n_connected; i++) {
            double key = lat[i];
            int j = i - 1;
            while (j >= 0 && lat[j] > key) { lat[j+1] = lat[j]; j--; }
            lat[j+1] = key;
        }
        double p50 = n_connected ? lat[(int)(n_connected * 0.50)] : -1;
        double p95 = n_connected ? lat[(int)(n_connected * 0.95 < n_connected ? n_connected*0.95 : n_connected-1)] : -1;
        double p99 = n_connected ? lat[(int)(n_connected * 0.99 < n_connected ? n_connected*0.99 : n_connected-1)] : -1;

        long long target_bytes = (long long)target * bitrate_bps * hold_secs;
        double pct_of_target = target_bytes > 0 ? (100.0 * (double)total_bytes / (double)target_bytes) : 0.0;

        printf("%d,%d,%d,%d,%.2f,%.2f,%.2f,%lld,%lld,%lld,%lld,%lld,%.2f,%.2f\n",
               target, n_new, n_connected, n_failed, p50, p95, p99,
               total_bytes, total_attempts, total_errs, total_wb, target_bytes, pct_of_target, elapsed_connect_s);
        fflush(stdout);
        free(lat);

        already_started = target;
    }

    g_running = 0;
    return 0;
}
