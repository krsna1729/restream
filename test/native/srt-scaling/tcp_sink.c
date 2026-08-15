// TCP control comparison for the SRT scaling investigation: same
// checkpoint-ramp methodology as sink_bench.c/sender_bench.c (SRT), but
// over plain TCP. RTMP-only@1200 never failed anywhere in this
// investigation while srt-only repeatedly did on the same host/kernel --
// this isolates whether that's because TCP's kernel-managed per-connection
// socket/buffer model (every accept() gets its own dedicated kernel
// socket, automatically, no equivalent of SRT's shared-multiplexer
// default) is what makes the difference, independent of anything
// RTMP-protocol-specific.
//
// Usage: tcp_sink <port> <total_worker_threads>
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
#include <sys/epoll.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <errno.h>
#include <fcntl.h>

#define MAX_EPOLL_EVENTS 4096
#define RECV_BUF_SIZE 65536

static void pin_to_cpu(int cpu) {
    int nproc = sysconf(_SC_NPROCESSORS_ONLN);
    if (nproc <= 0) return;
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu % nproc, &set);
    pthread_setaffinity_np(pthread_self(), sizeof(set), &set);
}

typedef struct {
    int thread_idx;
    int epfd;
    _Atomic long long bytes_received;
    _Atomic long long messages_received;
    _Atomic long long recv_errors;
} worker_ctx_t;

static _Atomic long long g_total_connections = 0;
static _Atomic int g_running = 1;
static worker_ctx_t *g_workers;
static int g_total_worker_threads;
static _Atomic int g_next_worker = 0;

static void *worker_thread(void *arg) {
    worker_ctx_t *ctx = (worker_ctx_t *)arg;
    pin_to_cpu(ctx->thread_idx);
    char *buf = malloc(RECV_BUF_SIZE);
    struct epoll_event events[MAX_EPOLL_EVENTS];

    while (g_running) {
        int n = epoll_wait(ctx->epfd, events, MAX_EPOLL_EVENTS, 100);
        if (n < 0) continue;
        for (int i = 0; i < n; i++) {
            int fd = events[i].data.fd;
            ssize_t r = recv(fd, buf, RECV_BUF_SIZE, 0);
            if (r > 0) {
                atomic_fetch_add(&ctx->bytes_received, r);
                atomic_fetch_add(&ctx->messages_received, 1);
            } else if (r == 0 || (r < 0 && errno != EAGAIN && errno != EWOULDBLOCK)) {
                epoll_ctl(ctx->epfd, EPOLL_CTL_DEL, fd, NULL);
                close(fd);
                atomic_fetch_add(&ctx->recv_errors, 1);
            }
        }
    }
    free(buf);
    return NULL;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <port> <total_worker_threads>\n", argv[0]);
        return 1;
    }
    int port = atoi(argv[1]);
    g_total_worker_threads = atoi(argv[2]);

    int listener = socket(AF_INET, SOCK_STREAM, 0);
    int reuse = 1;
    setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));
    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = INADDR_ANY;
    addr.sin_port = htons(port);
    if (bind(listener, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        perror("bind"); return 1;
    }
    if (listen(listener, 1024) < 0) {
        perror("listen"); return 1;
    }

    g_workers = calloc(g_total_worker_threads, sizeof(worker_ctx_t));
    pthread_t *threads = calloc(g_total_worker_threads, sizeof(pthread_t));
    for (int t = 0; t < g_total_worker_threads; t++) {
        g_workers[t].thread_idx = t;
        g_workers[t].epfd = epoll_create1(0);
        pthread_create(&threads[t], NULL, worker_thread, &g_workers[t]);
    }

    fprintf(stderr, "[tcp_sink] listening on port %d with %d worker threads\n", port, g_total_worker_threads);

    time_t last_report = time(NULL);
    while (g_running) {
        struct sockaddr_in peer;
        socklen_t peer_len = sizeof(peer);
        int fd = accept(listener, (struct sockaddr *)&peer, &peer_len);
        if (fd >= 0) {
            int nodelay = 1;
            setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &nodelay, sizeof(nodelay));
            int flags = fcntl(fd, F_GETFL, 0);
            fcntl(fd, F_SETFL, flags | O_NONBLOCK);
            int w = atomic_fetch_add(&g_next_worker, 1) % g_total_worker_threads;
            struct epoll_event ev = {0};
            ev.events = EPOLLIN;
            ev.data.fd = fd;
            epoll_ctl(g_workers[w].epfd, EPOLL_CTL_ADD, fd, &ev);
            atomic_fetch_add(&g_total_connections, 1);
        } else if (errno != EAGAIN && errno != EWOULDBLOCK) {
            usleep(1000);
        }

        time_t now = time(NULL);
        if (now != last_report) {
            long long total_bytes = 0, total_msgs = 0, total_errs = 0;
            for (int i = 0; i < g_total_worker_threads; i++) {
                total_bytes += atomic_load(&g_workers[i].bytes_received);
                total_msgs += atomic_load(&g_workers[i].messages_received);
                total_errs += atomic_load(&g_workers[i].recv_errors);
            }
            fprintf(stderr, "[tcp_sink] t=%ld connections=%lld bytes=%lld msgs=%lld errs=%lld\n",
                    (long)now, (long long)atomic_load(&g_total_connections), total_bytes, total_msgs, total_errs);
            last_report = now;
        }
    }
    return 0;
}
