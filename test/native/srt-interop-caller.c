// Minimal plain (non-bonded) SRT caller against stock libsrt, for Phase 3
// wire-interop testing of the pure-Rust SRT core (crates/srt-protocol) --
// see docs/srt-pure-rust-plan.md Phase 3 and the sibling
// srt-interop-listener.c.
//
// With a passphrase, sends the same known test payload
// srt-interop-caller.rs does -- used to live-verify the receiving side's
// crypto stack decrypts correctly against real libsrt, not just that the
// handshake completes.
//
// Usage: srt-interop-caller <host> <port> [stream_id] [passphrase]
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <srt/srt.h>

// Must match TEST_PAYLOAD in crates/srt-interop/src/bin/caller.rs.
static const char *TEST_PAYLOAD = "the quick brown fox jumps over the lazy dog 0123456789";

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <host> <port> [stream_id] [passphrase]\n", argv[0]);
        return 2;
    }
    const char *host = argv[1];
    const char *port = argv[2];
    const char *stream_id = (argc >= 4 && argv[3][0] != '\0') ? argv[3] : NULL;
    const char *passphrase = (argc >= 5 && argv[4][0] != '\0') ? argv[4] : NULL;
    setvbuf(stdout, NULL, _IONBF, 0);

    srt_startup();

    SRTSOCKET sock = srt_create_socket();
    if (sock == SRT_INVALID_SOCK) {
        fprintf(stderr, "srt_create_socket failed: %s\n", srt_getlasterror_str());
        return 1;
    }

    if (stream_id) {
        if (srt_setsockflag(sock, SRTO_STREAMID, stream_id, (int)strlen(stream_id)) == SRT_ERROR) {
            fprintf(stderr, "setsockflag STREAMID failed: %s\n", srt_getlasterror_str());
            return 1;
        }
    }
    if (passphrase) {
        if (srt_setsockflag(sock, SRTO_PASSPHRASE, passphrase, (int)strlen(passphrase)) == SRT_ERROR) {
            fprintf(stderr, "setsockflag PASSPHRASE failed: %s\n", srt_getlasterror_str());
            return 1;
        }
    }

    struct sockaddr_in address;
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_port = htons((uint16_t)atoi(port));
    inet_pton(AF_INET, host, &address.sin_addr);

    if (srt_connect(sock, (struct sockaddr *)&address, sizeof(address)) == SRT_ERROR) {
        fprintf(stderr, "srt_connect failed: %s\n", srt_getlasterror_str());
        return 1;
    }

    printf("CONNECTED\n");

    const char *msg = passphrase ? TEST_PAYLOAD : "hello from libsrt caller";
    srt_send(sock, msg, (int)strlen(msg));
    // Give the peer time to receive and decrypt before this process exits.
    usleep(300000);

    srt_close(sock);
    srt_cleanup();
    return 0;
}
