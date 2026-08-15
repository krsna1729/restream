// Parameterized SRT receiver for exploring the sink-side scaling design
// space. Three independent, composable dimensions:
//   - port_count: how many independent kernel UDP sockets (listener ports).
//     The ONLY way to get >1 independent kernel receive buffer through
//     libsrt, since it never sets SO_REUSEPORT (confirmed against the
//     vendored source: channel.cpp/socketconfig.cpp only ever set
//     SO_REUSEADDR). port_count==requested connection count is the extreme
//     "one port per destination" design: every stream gets full kernel-level
//     isolation, no shared queue, no risk of one connection's handshake/data
//     being delayed behind another's in the same socket buffer.
//   - total_worker_threads: CPU parallelism draining accepted connections,
//     decoupled from port_count. A global round-robin pool, NOT one pool per
//     listener -- at port_count=1200 you still want e.g. 4-8 threads, not
//     1200 OS threads each blocked in their own epoll_wait.
//   - udp_rcvbuf_bytes / srt_rcvbuf_bytes: kernel vs SRT-layer buffer size
//     per listener socket. At high port_count, each socket only ever
//     carries ONE stream's worth of traffic, so both can shrink far below
//     what a single shared socket needs for N streams.
//
// Usage: sink_bench <port_base> <port_count> <total_worker_threads> <udp_rcvbuf_bytes> [srt_rcvbuf_bytes]
//   udp_rcvbuf_bytes: 0 => don't set SRTO_UDP_RCVBUF (libsrt's own derived default)
//   srt_rcvbuf_bytes: 0 or omitted => don't set SRTO_RCVBUF (libsrt's 12MB-ish packet-derived default)
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
#include <syslog.h>

static void pin_to_cpu(int cpu) {
    int nproc = sysconf(_SC_NPROCESSORS_ONLN);
    if (nproc <= 0) return;
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu % nproc, &set);
    pthread_setaffinity_np(pthread_self(), sizeof(set), &set);
}

#define MAX_EPOLL_EVENTS 4096
#define RECV_BUF_SIZE 65536

typedef struct {
    int thread_idx;
    int eid;
    _Atomic long long bytes_received;
    _Atomic long long messages_received;
    _Atomic long long recv_errors;
} worker_ctx_t;

typedef struct {
    SRTSOCKET sock;
    int port;
} listener_ctx_t;

static _Atomic long long g_total_connections = 0;
static _Atomic int g_running = 1;
static _Atomic int g_next_worker = 0;

static void *worker_thread(void *arg) {
    worker_ctx_t *ctx = (worker_ctx_t *)arg;
    pin_to_cpu(ctx->thread_idx);
    char *buf = malloc(RECV_BUF_SIZE);
    SRTSOCKET readfds[MAX_EPOLL_EVENTS];

    while (g_running) {
        int rnum = MAX_EPOLL_EVENTS;
        int result = srt_epoll_wait(ctx->eid, readfds, &rnum, NULL, NULL, 100, NULL, NULL, NULL, NULL);
        if (result < 0) {
            continue;
        }
        for (int i = 0; i < rnum; i++) {
            SRT_MSGCTRL mc = srt_msgctrl_default;
            int n = srt_recvmsg2(readfds[i], buf, RECV_BUF_SIZE, &mc);
            if (n > 0) {
                atomic_fetch_add(&ctx->bytes_received, n);
                atomic_fetch_add(&ctx->messages_received, 1);
            } else {
                srt_epoll_remove_usock(ctx->eid, readfds[i]);
                srt_close(readfds[i]);
                atomic_fetch_add(&ctx->recv_errors, 1);
            }
        }
    }
    free(buf);
    return NULL;
}

int main(int argc, char **argv) {
    if (argc < 5) {
        fprintf(stderr, "usage: %s <port_base> <port_count> <total_worker_threads> <udp_rcvbuf_bytes> [srt_rcvbuf_bytes]\n", argv[0]);
        return 1;
    }
    int port_base = atoi(argv[1]);
    int port_count = atoi(argv[2]);
    int total_worker_threads = atoi(argv[3]);
    int udp_rcvbuf = atoi(argv[4]);
    int srt_rcvbuf = (argc >= 6) ? atoi(argv[5]) : 0;

    srt_startup();
    srt_setloglevel(LOG_CRIT);

    listener_ctx_t *listeners = calloc(port_count, sizeof(listener_ctx_t));
    worker_ctx_t *workers = calloc(total_worker_threads, sizeof(worker_ctx_t));
    pthread_t *threads = calloc(total_worker_threads, sizeof(pthread_t));

    for (int t = 0; t < total_worker_threads; t++) {
        workers[t].thread_idx = t;
        workers[t].eid = srt_epoll_create();
        pthread_create(&threads[t], NULL, worker_thread, &workers[t]);
    }

    for (int p = 0; p < port_count; p++) {
        SRTSOCKET listener = srt_create_socket();
        int no = 0;
        srt_setsockopt(listener, 0, SRTO_RCVSYN, &no, sizeof(no));
        int fc = 32768;
        srt_setsockopt(listener, 0, SRTO_FC, &fc, sizeof(fc));
        if (srt_rcvbuf > 0) {
            srt_setsockopt(listener, 0, SRTO_RCVBUF, &srt_rcvbuf, sizeof(srt_rcvbuf));
        }
        if (udp_rcvbuf > 0) {
            srt_setsockopt(listener, 0, SRTO_UDP_RCVBUF, &udp_rcvbuf, sizeof(udp_rcvbuf));
        }

        struct sockaddr_in sa;
        memset(&sa, 0, sizeof(sa));
        sa.sin_family = AF_INET;
        sa.sin_port = htons(port_base + p);
        sa.sin_addr.s_addr = INADDR_ANY;
        if (srt_bind(listener, (struct sockaddr *)&sa, sizeof(sa)) == SRT_ERROR) {
            fprintf(stderr, "bind failed on port %d: %s\n", port_base + p, srt_getlasterror_str());
            return 1;
        }
        if (srt_listen(listener, 128) == SRT_ERROR) {
            fprintf(stderr, "listen failed on port %d: %s\n", port_base + p, srt_getlasterror_str());
            return 1;
        }
        listeners[p].sock = listener;
        listeners[p].port = port_base + p;
    }

    fprintf(stderr, "[sink_bench] port_base=%d port_count=%d total_worker_threads=%d udp_rcvbuf=%d srt_rcvbuf=%d listening\n",
            port_base, port_count, total_worker_threads, udp_rcvbuf, srt_rcvbuf);

    time_t last_report = time(NULL);
    while (g_running) {
        int any_accepted = 0;
        for (int p = 0; p < port_count; p++) {
            struct sockaddr_in peer;
            int peer_len = sizeof(peer);
            SRTSOCKET s = srt_accept(listeners[p].sock, (struct sockaddr *)&peer, &peer_len);
            if (s != SRT_INVALID_SOCK) {
                int no2 = 0;
                srt_setsockopt(s, 0, SRTO_RCVSYN, &no2, sizeof(no2));
                int events = SRT_EPOLL_IN | SRT_EPOLL_ERR;
                int w = atomic_fetch_add(&g_next_worker, 1) % total_worker_threads;
                srt_epoll_add_usock(workers[w].eid, s, &events);
                atomic_fetch_add(&g_total_connections, 1);
                any_accepted = 1;
            }
        }
        if (!any_accepted) {
            usleep(1000);
        }

        time_t now = time(NULL);
        if (now != last_report) {
            long long total_bytes = 0, total_msgs = 0, total_errs = 0;
            for (int i = 0; i < total_worker_threads; i++) {
                total_bytes += atomic_load(&workers[i].bytes_received);
                total_msgs += atomic_load(&workers[i].messages_received);
                total_errs += atomic_load(&workers[i].recv_errors);
            }
            fprintf(stderr, "[sink_bench] t=%ld connections=%lld bytes=%lld msgs=%lld errs=%lld\n",
                    (long)now, (long long)atomic_load(&g_total_connections), total_bytes, total_msgs, total_errs);
            last_report = now;
        }
    }
    return 0;
}
