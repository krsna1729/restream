// Sustained-throughput SRT receiver against stock libsrt, for
// docs/srt-pure-rust-plan.md Phase 4's differential loss/latency testing --
// see the sibling srt-loss-caller.c. Unlike srt-interop-listener.c (a Phase
// 3 one-shot wire-interop check), this receives for a configured duration
// under injected network impairment (tc netem, applied by the caller
// script) and reports libsrt's own srt_bistats() counters at the end, so
// they can be compared directly against crates/srt-interop's Rust
// equivalent under the identical impairment.
//
// Usage: srt-loss-listener <port> <duration_seconds> <latency_ms>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sys/resource.h>
#include <srt/srt.h>

#define PAYLOAD_SIZE 1316

static double now_seconds(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: %s <port> <duration_seconds> <latency_ms>\n", argv[0]);
        return 2;
    }
    int port = atoi(argv[1]);
    double duration = atof(argv[2]);
    int latency_ms = atoi(argv[3]);
    setvbuf(stdout, NULL, _IONBF, 0);

    srt_startup();

    SRTSOCKET listener = srt_create_socket();
    if (listener == SRT_INVALID_SOCK) {
        fprintf(stderr, "srt_create_socket failed: %s\n", srt_getlasterror_str());
        return 1;
    }

    int payload_size = PAYLOAD_SIZE;
    srt_setsockflag(listener, SRTO_PAYLOADSIZE, &payload_size, sizeof(payload_size));
    srt_setsockflag(listener, SRTO_LATENCY, &latency_ms, sizeof(latency_ms));

    struct sockaddr_in address;
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_port = htons((uint16_t)port);
    address.sin_addr.s_addr = htonl(INADDR_ANY);

    if (srt_bind(listener, (struct sockaddr *)&address, sizeof(address)) == SRT_ERROR) {
        fprintf(stderr, "srt_bind failed: %s\n", srt_getlasterror_str());
        return 1;
    }
    if (srt_listen(listener, 1) == SRT_ERROR) {
        fprintf(stderr, "srt_listen failed: %s\n", srt_getlasterror_str());
        return 1;
    }

    printf("LISTENING\n");

    struct sockaddr_in peer_addr;
    int peer_len = sizeof(peer_addr);
    SRTSOCKET sock = srt_accept(listener, (struct sockaddr *)&peer_addr, &peer_len);
    if (sock == SRT_INVALID_SOCK) {
        fprintf(stderr, "srt_accept failed: %s\n", srt_getlasterror_str());
        return 1;
    }

    printf("CONNECTED\n");

    char buf[PAYLOAD_SIZE];
    long long total_recv = 0;
    double start = now_seconds();
    double deadline = start + duration;

    // Poll with a short recv timeout so we notice the wall-clock deadline
    // even during a total-loss stretch, rather than blocking forever.
    int recv_timeout_ms = 200;
    srt_setsockflag(sock, SRTO_RCVTIMEO, &recv_timeout_ms, sizeof(recv_timeout_ms));

    // Snapshot stats on every iteration (not just once after the loop
    // exits): the caller process runs the same nominal duration and closes
    // its socket right after, so a single post-loop srt_bistats() call here
    // can race against that close and fail with "Connection was broken",
    // discarding real data. Keeping a rolling snapshot from while the
    // connection was still definitely alive avoids the race entirely --
    // the last snapshot is at most one recv-timeout tick (200ms) stale.
    SRT_TRACEBSTATS stats;
    memset(&stats, 0, sizeof(stats));

    while (now_seconds() < deadline) {
        int n = srt_recv(sock, buf, sizeof(buf));
        if (n > 0) {
            total_recv++;
        } else if (n == SRT_ERROR) {
            int err = srt_getlasterror(NULL);
            if (err == SRT_ETIMEOUT) {
                srt_bistats(sock, &stats, 0, 1);
                continue;
            }
            // Connection closed or errored -- stop early, report what we have.
            break;
        }
        srt_bistats(sock, &stats, 0, 1);
    }

    // getrusage(RUSAGE_SELF, ...): same CPU/memory accounting the Rust
    // driver-framework bake-off binaries use (crates/srt-interop/src/
    // cpu_stats.rs), so libsrt can serve as a complete reference baseline
    // on all three axes -- throughput, latency, and CPU/memory -- not just
    // throughput/RTT.
    struct rusage usage;
    memset(&usage, 0, sizeof(usage));
    getrusage(RUSAGE_SELF, &usage);
    double cpu_user_ms = usage.ru_utime.tv_sec * 1000.0 + usage.ru_utime.tv_usec / 1000.0;
    double cpu_sys_ms = usage.ru_stime.tv_sec * 1000.0 + usage.ru_stime.tv_usec / 1000.0;

    printf(
        "STATS role=listener backend=libsrt pkt_recv=%lld pkt_recv_total=%lld "
        "pkt_rcv_loss_total=%d pkt_rcv_drop_total=%d pkt_retrans_total=%d "
        "rtt_ms=%.3f elapsed_s=%.3f cpu_user_ms=%.1f cpu_sys_ms=%.1f peak_rss_kb=%ld\n",
        total_recv, (long long)stats.pktRecv, stats.pktRcvLossTotal,
        stats.pktRcvDropTotal, stats.pktRetransTotal, stats.msRTT,
        now_seconds() - start, cpu_user_ms, cpu_sys_ms, usage.ru_maxrss);

    srt_close(sock);
    srt_close(listener);
    srt_cleanup();
    return 0;
}
