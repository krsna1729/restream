// TCP tier of the TCP vs UDP vs SRT scaling ladder. Per-connection deadline
// pacing (not round-scoped -- an earlier version paced per-round across all
// of a thread's connections, silently under-delivering) and per-thread
// exclusive connection ownership (not a shared array filtered by owner --
// that was Nthreads x redundant scans plus cross-thread cache contention),
// same fixes as sender_bench.c (SRT) and udp_sender.c, for a fair three-way
// comparison.
//
// Usage: tcp_sender <host> <port> <threads> <bitrate_Bps> <c1,c2,...> <hold_secs>
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <unistd.h>
#include <time.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>

#define PAYLOAD_SIZE 1316

static void pin_to_cpu(int cpu) {
    int nproc = sysconf(_SC_NPROCESSORS_ONLN);
    if (nproc <= 0) return;
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu % nproc, &set);
    pthread_setaffinity_np(pthread_self(), sizeof(set), &set);
}

typedef struct {
    int fd;
    struct timespec next_due;
} owned_conn_t;

typedef struct {
    int thread_id;
    owned_conn_t *owned;
    _Atomic int n_owned;
    _Atomic long long bytes_sent;
    _Atomic long long send_attempts;
    _Atomic long long send_errors;
    _Atomic long long send_would_block;
} worker_ctx_t;

static double g_interval_ms;
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
        double min_wait_ms = 1.0;
        for (int i = 0; i < n; i++) {
            owned_conn_t *c = &ctx->owned[i];
            double due_in_ms = ts_diff_ms(&now, &c->next_due);
            if (due_in_ms > 0) {
                if (due_in_ms < min_wait_ms) min_wait_ms = due_in_ms;
                continue;
            }
            atomic_fetch_add(&ctx->send_attempts, 1);
            ssize_t r = send(c->fd, payload, PAYLOAD_SIZE, MSG_NOSIGNAL | MSG_DONTWAIT);
            if (r > 0) {
                atomic_fetch_add(&ctx->bytes_sent, r);
                ts_add_ms(&c->next_due, g_interval_ms);
                if (ts_diff_ms(&c->next_due, &now) > 0) c->next_due = now;
            } else if (r < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
                atomic_fetch_add(&ctx->send_would_block, 1);
            } else {
                atomic_fetch_add(&ctx->send_errors, 1);
            }
            min_wait_ms = 0.0;
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
    if (argc < 7) {
        fprintf(stderr, "usage: %s <host> <port> <threads> <bitrate_Bps> <c1,c2,...> <hold_secs>\n", argv[0]);
        return 1;
    }
    const char *host = argv[1];
    int port = atoi(argv[2]);
    int nthreads = atoi(argv[3]);
    long bitrate_bps = atol(argv[4]);
    int hold_secs = atoi(argv[6]);
    g_interval_ms = (double)PAYLOAD_SIZE * 1000.0 / (double)bitrate_bps;

    int checkpoints[64];
    int nckpt = 0;
    char *ckpt_str = strdup(argv[5]);
    char *tok = strtok(ckpt_str, ",");
    while (tok && nckpt < 64) {
        checkpoints[nckpt++] = atoi(tok);
        tok = strtok(NULL, ",");
    }
    int max_conns = checkpoints[nckpt - 1];

    worker_ctx_t *workers = calloc(nthreads, sizeof(worker_ctx_t));
    pthread_t *threads = calloc(nthreads, sizeof(pthread_t));
    for (int i = 0; i < nthreads; i++) {
        workers[i].thread_id = i;
        workers[i].owned = calloc(max_conns, sizeof(owned_conn_t));
        atomic_store(&workers[i].n_owned, 0);
        pthread_create(&threads[i], NULL, worker_thread, &workers[i]);
    }

    struct sockaddr_in dst = {0};
    dst.sin_family = AF_INET;
    dst.sin_port = htons(port);
    inet_pton(AF_INET, host, &dst.sin_addr);

    printf("checkpoint,requested,connected,failed,connect_p50_ms,connect_p95_ms,connect_p99_ms,steady_bytes_sent,steady_send_attempts,steady_send_errors,steady_would_block,target_bytes,pct_of_target,elapsed_connect_s\n");

    int already_started = 0;
    for (int c = 0; c < nckpt; c++) {
        int target = checkpoints[c];
        struct timespec ramp_start, ramp_end;
        clock_gettime(CLOCK_MONOTONIC, &ramp_start);

        int n_new = target - already_started;
        double *lat = malloc(sizeof(double) * (n_new > 0 ? n_new : 1));
        int connected = 0, failed = 0;

        for (int idx = already_started; idx < target; idx++) {
            int fd = socket(AF_INET, SOCK_STREAM, 0);
            struct timespec t0, t1;
            clock_gettime(CLOCK_MONOTONIC, &t0);
            int rc = connect(fd, (struct sockaddr *)&dst, sizeof(dst));
            clock_gettime(CLOCK_MONOTONIC, &t1);
            if (rc < 0) {
                close(fd);
                failed++;
                continue;
            }
            int nodelay = 1;
            setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &nodelay, sizeof(nodelay));
            int flags = fcntl(fd, F_GETFL, 0);
            fcntl(fd, F_SETFL, flags | O_NONBLOCK);

            lat[connected++] = ts_diff_ms(&t0, &t1);

            int owner = idx % nthreads;
            worker_ctx_t *w = &workers[owner];
            int slot = atomic_load_explicit(&w->n_owned, memory_order_relaxed);
            w->owned[slot].fd = fd;
            w->owned[slot].next_due = t1;
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

        for (int i = 1; i < connected; i++) {
            double key = lat[i];
            int j = i - 1;
            while (j >= 0 && lat[j] > key) { lat[j+1] = lat[j]; j--; }
            lat[j+1] = key;
        }
        double p50 = connected ? lat[(int)(connected * 0.50)] : -1;
        double p95 = connected ? lat[(int)(connected * 0.95 < connected ? connected*0.95 : connected-1)] : -1;
        double p99 = connected ? lat[(int)(connected * 0.99 < connected ? connected*0.99 : connected-1)] : -1;

        long long target_bytes = (long long)target * bitrate_bps * hold_secs;
        double pct_of_target = target_bytes > 0 ? (100.0 * (double)total_bytes / (double)target_bytes) : 0.0;

        printf("%d,%d,%d,%d,%.2f,%.2f,%.2f,%lld,%lld,%lld,%lld,%lld,%.2f,%.2f\n",
               target, n_new, connected, failed, p50, p95, p99,
               total_bytes, total_attempts, total_errs, total_wb, target_bytes, pct_of_target, elapsed_connect_s);
        fflush(stdout);
        free(lat);

        already_started = target;
    }

    g_running = 0;
    return 0;
}
