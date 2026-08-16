// Minimal plain (non-bonded) SRT listener against stock libsrt, for Phase 3
// wire-interop testing of the pure-Rust SRT core (crates/srt-protocol) --
// see docs/srt-pure-rust-plan.md Phase 3 and the sibling
// srt-interop-caller.c.
//
// Usage: srt-interop-listener <port> [passphrase]
//
// With a passphrase, SRTO_PASSPHRASE is required on the listener socket --
// a caller that doesn't supply one is rejected by libsrt itself at the
// protocol level (SRT_REJ_UNSECURE), before srt_accept ever returns. Used
// to live-verify the Rust core's reject-reason handling: see
// docs/srt-pure-rust-plan.md Phase 3.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <srt/srt.h>

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <port> [passphrase]\n", argv[0]);
        return 2;
    }
    int port = atoi(argv[1]);
    const char *passphrase = argc >= 3 ? argv[2] : NULL;
    setvbuf(stdout, NULL, _IONBF, 0);

    srt_startup();

    SRTSOCKET listener = srt_create_socket();
    if (listener == SRT_INVALID_SOCK) {
        fprintf(stderr, "srt_create_socket failed: %s\n", srt_getlasterror_str());
        return 1;
    }

    if (passphrase) {
        if (srt_setsockflag(listener, SRTO_PASSPHRASE, passphrase, (int)strlen(passphrase)) == SRT_ERROR) {
            fprintf(stderr, "setsockflag PASSPHRASE failed: %s\n", srt_getlasterror_str());
            return 1;
        }
    }

    struct sockaddr_in address;
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_port = htons((uint16_t)port);
    address.sin_addr.s_addr = INADDR_ANY;

    if (srt_bind(listener, (struct sockaddr *)&address, sizeof(address)) == SRT_ERROR) {
        fprintf(stderr, "srt_bind failed: %s\n", srt_getlasterror_str());
        return 1;
    }
    if (srt_listen(listener, 2) == SRT_ERROR) {
        fprintf(stderr, "srt_listen failed: %s\n", srt_getlasterror_str());
        return 1;
    }

    fprintf(stderr, "[srt-interop-listener] listening on port %d%s\n", port,
            passphrase ? " (passphrase required)" : "");

    struct sockaddr_in peer_addr;
    int peer_addr_len = sizeof(peer_addr);
    SRTSOCKET accepted = srt_accept(listener, (struct sockaddr *)&peer_addr, &peer_addr_len);
    if (accepted == SRT_INVALID_SOCK) {
        fprintf(stderr, "srt_accept failed (expected if the caller was rejected): %s\n",
                srt_getlasterror_str());
        // The rejection response is already sent by the time srt_accept
        // returns; this delay is just so the test harness has a moment to
        // observe the caller-side result before this process exits.
        srt_close(listener);
        usleep(500000);
        srt_cleanup();
        return 1;
    }

    char stream_id[513] = {0};
    int sid_len = sizeof(stream_id) - 1;
    srt_getsockflag(accepted, SRTO_STREAMID, stream_id, &sid_len);

    printf("CONNECTED stream_id=%s\n", stream_id);
    fflush(stdout);

    char buf[2048];
    int n = srt_recv(accepted, buf, sizeof(buf));
    if (n > 0) {
        fprintf(stderr, "[srt-interop-listener] received %d bytes\n", n);
    }

    srt_close(accepted);
    srt_close(listener);
    srt_cleanup();
    return 0;
}
