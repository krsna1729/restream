// Raw-UDP receiver, two selectable kernel receive-path modes -- the actual
// testing ground for "4-tuple listener vs established (connect()ed) socket"
// on the UDP receive side, independent of any SRT protocol behavior:
//
//   shared:    port_count plain UDP sockets, each fielding recvfrom() from
//              however many peers land on it. This is the shape a single
//              SRT multiplexer's underlying kernel socket has: one shared
//              receive queue for every flow on that port.
//   connected: port_count "greeter" sockets detect new peers via recvfrom,
//              then each peer gets its own dedicated connect()ed UDP
//              socket (SO_REUSEADDR + bind(same local port) + connect(peer)),
//              so the kernel's 4-tuple hash lookup routes that peer's
//              traffic to its own socket/buffer from then on -- the same
//              mechanism validated for libsrt in the patched-fork
//              investigation (see docs/agent-guidance/quality/
//              srt-scaling-first-principles-investigation-2026-08-15.md),
//              tested here against plain UDP with zero protocol overhead.
//
// Usage: udp_sink <port_base> <port_count> <total_worker_threads> <rcvbuf_bytes> <mode: shared|connected>
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

static void pin_to_cpu(int cpu) {
    int nproc = sysconf(_SC_NPROCESSORS_ONLN);
    if (nproc <= 0) return;
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu % nproc, &set);
    pthread_setaffinity_np(pthread_self(), sizeof(set), &set);
}
#include <arpa/inet.h>
#include <errno.h>

#define MAX_EPOLL_EVENTS 4096
#define RECV_BUF_SIZE 65536
#define MAX_PEERS_PER_LISTENER 4096

typedef struct {
    int fd;
    _Atomic long long bytes_received;
    _Atomic long long messages_received;
} worker_ctx_t;

typedef struct {
    int fd;
    int port;
    // Known peers on this listener, for connected-mode dedup (linear scan;
    // fine at benchmark scale, not a production pattern).
    struct sockaddr_in peers[MAX_PEERS_PER_LISTENER];
    int n_peers;
} listener_ctx_t;

static _Atomic long long g_total_connections = 0;
static _Atomic int g_running = 1;
static _Atomic int g_next_worker = 0;
static int g_total_worker_threads;
static worker_ctx_t *g_workers;
static int *g_worker_epfds;

// Each worker thread owns one epoll fd (stored in g_worker_epfds),
// polling however many sockets (shared listeners, or per-peer connected
// sockets) have been added to it.
static int g_cpu_base = 2;

static void *worker_loop(void *arg) {
    long idx = (long)arg;
    pin_to_cpu(g_cpu_base + (int)idx);
    int epfd = g_worker_epfds[idx];
    char *buf = malloc(RECV_BUF_SIZE);
    struct epoll_event events[MAX_EPOLL_EVENTS];

    while (g_running) {
        // Busy-poll: timeout=0 spins instead of blocking on the kernel
        // waking this thread. SO_BUSY_POLL itself targets NAPI-polling a
        // real NIC ring buffer and has no equivalent benefit on loopback
        // (no interrupt-driven NIC to poll instead of), but the same
        // "spin instead of sleep" principle still removes wakeup latency
        // from a blocking epoll_wait() call, which does apply here.
        int n = epoll_wait(epfd, events, MAX_EPOLL_EVENTS, 0);
        if (n <= 0) continue;
        for (int i = 0; i < n; i++) {
            int fd = events[i].data.fd;
            for (;;) {
                ssize_t r = recv(fd, buf, RECV_BUF_SIZE, MSG_DONTWAIT);
                if (r > 0) {
                    atomic_fetch_add(&g_workers[idx].bytes_received, r);
                    atomic_fetch_add(&g_workers[idx].messages_received, 1);
                } else {
                    break;
                }
            }
        }
    }
    free(buf);
    return NULL;
}

int main(int argc, char **argv) {
    if (argc < 6) {
        fprintf(stderr, "usage: %s <port_base> <port_count> <total_worker_threads> <rcvbuf_bytes> <mode: shared|connected>\n", argv[0]);
        return 1;
    }
    int port_base = atoi(argv[1]);
    int port_count = atoi(argv[2]);
    g_total_worker_threads = atoi(argv[3]);
    int rcvbuf = atoi(argv[4]);
    int connected_mode = strcmp(argv[5], "connected") == 0;
    if (argc >= 7) g_cpu_base = atoi(argv[6]);

    g_workers = calloc(g_total_worker_threads, sizeof(worker_ctx_t));
    g_worker_epfds = calloc(g_total_worker_threads, sizeof(int));
    pthread_t *threads = calloc(g_total_worker_threads, sizeof(pthread_t));
    for (int t = 0; t < g_total_worker_threads; t++) {
        g_worker_epfds[t] = epoll_create1(0);
        pthread_create(&threads[t], NULL, worker_loop, (void *)(long)t);
    }

    listener_ctx_t *listeners = calloc(port_count, sizeof(listener_ctx_t));
    for (int p = 0; p < port_count; p++) {
        int fd = socket(AF_INET, SOCK_DGRAM, 0);
        int reuse = 1;
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));
        if (rcvbuf > 0) setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &rcvbuf, sizeof(rcvbuf));
        struct sockaddr_in sa = {0};
        sa.sin_family = AF_INET;
        sa.sin_port = htons(port_base + p);
        sa.sin_addr.s_addr = INADDR_ANY;
        if (bind(fd, (struct sockaddr *)&sa, sizeof(sa)) < 0) {
            perror("bind"); return 1;
        }
        listeners[p].fd = fd;
        listeners[p].port = port_base + p;
        listeners[p].n_peers = 0;

        if (!connected_mode) {
            // Shared mode: register the listener itself directly; every
            // peer's traffic lands in this one socket's receive queue.
            int w = atomic_fetch_add(&g_next_worker, 1) % g_total_worker_threads;
            struct epoll_event ev = {0};
            ev.events = EPOLLIN;
            ev.data.fd = fd;
            epoll_ctl(g_worker_epfds[w], EPOLL_CTL_ADD, fd, &ev);
        }
    }

    fprintf(stderr, "[udp_sink] port_base=%d port_count=%d threads=%d rcvbuf=%d mode=%s listening\n",
            port_base, port_count, g_total_worker_threads, rcvbuf, connected_mode ? "connected" : "shared");

    char discard_buf[RECV_BUF_SIZE];
    time_t last_report = time(NULL);
    while (g_running) {
        int any = 0;
        if (connected_mode) {
            for (int p = 0; p < port_count; p++) {
                struct sockaddr_in peer;
                socklen_t peer_len = sizeof(peer);
                ssize_t r = recvfrom(listeners[p].fd, discard_buf, sizeof(discard_buf),
                                      MSG_DONTWAIT, (struct sockaddr *)&peer, &peer_len);
                if (r <= 0) continue;
                any = 1;

                int known = 0;
                for (int i = 0; i < listeners[p].n_peers; i++) {
                    if (listeners[p].peers[i].sin_addr.s_addr == peer.sin_addr.s_addr &&
                        listeners[p].peers[i].sin_port == peer.sin_port) {
                        known = 1;
                        break;
                    }
                }
                if (known || listeners[p].n_peers >= MAX_PEERS_PER_LISTENER) continue;

                listeners[p].peers[listeners[p].n_peers++] = peer;

                int pfd = socket(AF_INET, SOCK_DGRAM, 0);
                int reuse = 1;
                setsockopt(pfd, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));
                if (rcvbuf > 0) setsockopt(pfd, SOL_SOCKET, SO_RCVBUF, &rcvbuf, sizeof(rcvbuf));
                struct sockaddr_in local = {0};
                local.sin_family = AF_INET;
                local.sin_port = htons(listeners[p].port);
                local.sin_addr.s_addr = INADDR_ANY;
                bind(pfd, (struct sockaddr *)&local, sizeof(local));
                connect(pfd, (struct sockaddr *)&peer, sizeof(peer));

                int w = atomic_fetch_add(&g_next_worker, 1) % g_total_worker_threads;
                struct epoll_event ev = {0};
                ev.events = EPOLLIN;
                ev.data.fd = pfd;
                epoll_ctl(g_worker_epfds[w], EPOLL_CTL_ADD, pfd, &ev);
                atomic_fetch_add(&g_total_connections, 1);
            }
        } else {
            usleep(50000);
        }
        if (!any) usleep(1000);

        time_t now = time(NULL);
        if (now != last_report) {
            long long total_bytes = 0, total_msgs = 0;
            for (int i = 0; i < g_total_worker_threads; i++) {
                total_bytes += atomic_load(&g_workers[i].bytes_received);
                total_msgs += atomic_load(&g_workers[i].messages_received);
            }
            fprintf(stderr, "[udp_sink] t=%ld peers=%lld bytes=%lld msgs=%lld\n",
                    (long)now, (long long)atomic_load(&g_total_connections), total_bytes, total_msgs);
            last_report = now;
        }
    }
    return 0;
}
