// Sustained-throughput SRT sender against stock libsrt, paced to a target
// bitrate for a configured duration -- the caller-side counterpart to
// srt-loss-listener.c. See that file's header comment for context
// (docs/srt-pure-rust-plan.md Phase 4 differential loss/latency testing).
//
// Usage: srt-loss-caller <host> <port> <duration_seconds> <latency_ms> [bitrate_bps]
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <srt/srt.h>

#define PAYLOAD_SIZE 1316
#define DEFAULT_BITRATE_BPS 8000000

static double now_seconds(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static void sleep_seconds(double s) {
    if (s <= 0.0) {
        return;
    }
    struct timespec ts;
    ts.tv_sec = (time_t)s;
    ts.tv_nsec = (long)((s - (double)ts.tv_sec) * 1e9);
    nanosleep(&ts, NULL);
}

int main(int argc, char **argv) {
    if (argc < 5) {
        fprintf(stderr,
                "usage: %s <host> <port> <duration_seconds> <latency_ms> [bitrate_bps]\n",
                argv[0]);
        return 2;
    }
    const char *host = argv[1];
    int port = atoi(argv[2]);
    double duration = atof(argv[3]);
    int latency_ms = atoi(argv[4]);
    long bitrate_bps = (argc >= 6) ? atol(argv[5]) : DEFAULT_BITRATE_BPS;
    setvbuf(stdout, NULL, _IONBF, 0);

    double packet_interval_s = (double)(PAYLOAD_SIZE * 8) / (double)bitrate_bps;

    srt_startup();

    SRTSOCKET sock = srt_create_socket();
    if (sock == SRT_INVALID_SOCK) {
        fprintf(stderr, "srt_create_socket failed: %s\n", srt_getlasterror_str());
        return 1;
    }

    int payload_size = PAYLOAD_SIZE;
    srt_setsockflag(sock, SRTO_PAYLOADSIZE, &payload_size, sizeof(payload_size));
    srt_setsockflag(sock, SRTO_LATENCY, &latency_ms, sizeof(latency_ms));

    struct sockaddr_in address;
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_port = htons((uint16_t)port);
    inet_pton(AF_INET, host, &address.sin_addr);

    if (srt_connect(sock, (struct sockaddr *)&address, sizeof(address)) == SRT_ERROR) {
        fprintf(stderr, "srt_connect failed: %s\n", srt_getlasterror_str());
        return 1;
    }

    printf("CONNECTED\n");

    char payload[PAYLOAD_SIZE];
    memset(payload, 0x42, sizeof(payload));

    long long total_sent = 0;
    double start = now_seconds();
    double deadline = start + duration;
    double next_send = start;

    // Rolling snapshot, not a single post-loop query -- see the matching
    // comment in srt-loss-listener.c. The listener runs the same nominal
    // duration and may close first, which can otherwise race a final
    // srt_bistats() call here into "Connection was broken".
    SRT_TRACEBSTATS stats;
    memset(&stats, 0, sizeof(stats));

    while (now_seconds() < deadline) {
        double now = now_seconds();
        if (now < next_send) {
            sleep_seconds(next_send - now);
        }
        int n = srt_send(sock, payload, PAYLOAD_SIZE);
        if (n == SRT_ERROR) {
            int err = srt_getlasterror(NULL);
            if (err == SRT_ETIMEOUT || err == SRT_EASYNCSND) {
                // Send buffer full (congestion window / flow control) --
                // real backpressure, not a fatal error. Retry the next tick.
            } else {
                break;
            }
        } else {
            total_sent++;
        }
        next_send += packet_interval_s;
        srt_bistats(sock, &stats, 0, 1);
    }

    printf(
        "STATS role=caller pkt_sent=%lld pkt_sent_total=%lld "
        "pkt_snd_loss_total=%d pkt_retrans_total=%d rtt_ms=%.3f "
        "elapsed_s=%.3f\n",
        total_sent, (long long)stats.pktSent, stats.pktSndLossTotal,
        stats.pktRetransTotal, stats.msRTT, now_seconds() - start);

    srt_close(sock);
    srt_cleanup();
    return 0;
}
