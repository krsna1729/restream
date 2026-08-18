#include <arpa/inet.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <srt/srt.h>

int main(int argc, char **argv) {
    if (argc != 3 && argc != 4) {
        fprintf(stderr, "usage: %s <broadcast|backup> <port> [failover]\n", argv[0]);
        return 2;
    }
    if (argc == 4 && strcmp(argv[3], "failover") != 0) {
        fprintf(stderr, "unknown test action: %s\n", argv[3]);
        return 2;
    }
    int expected_messages = strcmp(argv[1], "backup") == 0 ? 2 : 1;
    int close_primary_after_first = argc == 4 && strcmp(argv[1], "backup") == 0;

    srt_startup();
    SRTSOCKET listener = srt_create_socket();
    int yes = 1;
    if (listener == SRT_INVALID_SOCK ||
        srt_setsockflag(listener, SRTO_GROUPCONNECT, &yes, sizeof(yes)) == SRT_ERROR) {
        fprintf(stderr, "group listener unavailable: %s\n", srt_getlasterror_str());
        return 1;
    }

    struct sockaddr_in address;
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_port = htons((uint16_t)atoi(argv[2]));
    inet_pton(AF_INET, "127.0.0.1", &address.sin_addr);

    if (srt_bind(listener, (struct sockaddr *)&address, sizeof(address)) == SRT_ERROR ||
        srt_listen(listener, 8) == SRT_ERROR) {
        fprintf(stderr, "listen failed: %s\n", srt_getlasterror_str());
        return 1;
    }

    int address_length = sizeof(address);
    if (srt_getsockname(listener, (struct sockaddr *)&address, &address_length) == SRT_ERROR) {
        fprintf(stderr, "getsockname failed: %s\n", srt_getlasterror_str());
        return 1;
    }
    printf("ready port=%u\n", (unsigned)ntohs(address.sin_port));
    fflush(stdout);

    struct sockaddr_storage peer;
    int peer_length = sizeof(peer);
    SRTSOCKET group = srt_accept(listener, (struct sockaddr *)&peer, &peer_length);
    if (group == SRT_INVALID_SOCK || !(group & SRTGROUP_MASK)) {
        fprintf(stderr, "listener did not accept a socket group: %s\n", srt_getlasterror_str());
        return 1;
    }

    SRT_SOCKGROUPDATA initial_members[8];
    size_t initial_member_count = 0;
    for (int attempt = 0; attempt < 100; ++attempt) {
        initial_member_count = 8;
        if (srt_group_data(group, initial_members, &initial_member_count) != SRT_ERROR &&
            initial_member_count >= 2)
            break;
        usleep(20000);
    }
    if (initial_member_count < 2) {
        fprintf(stderr, "expected two connected members, got %zu\n", initial_member_count);
        return 1;
    }

    int total_received = 0;
    for (int index = 0; index < expected_messages; ++index) {
        char message[2048];
        SRT_MSGCTRL message_control;
        srt_msgctrl_init(&message_control);
        int received = srt_recvmsg2(group, message, sizeof(message), &message_control);
        if (received <= 0) {
            fprintf(stderr, "group receive failed: %s\n", srt_getlasterror_str());
            return 1;
        }
        total_received += received;

        if (close_primary_after_first && index == 0) {
            SRT_SOCKGROUPDATA active_members[8];
            size_t active_count = 8;
            if (srt_group_data(group, active_members, &active_count) == SRT_ERROR) {
                fprintf(stderr, "failed to inspect group before failover: %s\n",
                        srt_getlasterror_str());
                return 1;
            }
            SRTSOCKET primary = SRT_INVALID_SOCK;
            for (size_t member_index = 0; member_index < active_count; ++member_index) {
                if (active_members[member_index].weight == 1) {
                    primary = active_members[member_index].id;
                    break;
                }
            }
            if (primary == SRT_INVALID_SOCK || srt_close(primary) == SRT_ERROR) {
                fprintf(stderr, "failed to close primary member for failover: %s\n",
                        srt_getlasterror_str());
                return 1;
            }
            printf("closed_primary_after_first=1\n");
            fflush(stdout);
        }
    }

    SRT_SOCKGROUPDATA members[8];
    size_t member_count = 8;
    for (int attempt = 0; attempt < 100; ++attempt) {
        member_count = 8;
        if (srt_group_data(group, members, &member_count) != SRT_ERROR && member_count >= 2)
            break;
        usleep(20000);
    }

    if (!close_primary_after_first && member_count < 2) {
        fprintf(stderr, "expected two group members, got %zu\n", member_count);
        return 1;
    }

    printf("accepted_group members_before=%zu members_after=%zu messages=%d bytes=%d\n",
           initial_member_count, member_count, expected_messages, total_received);
    srt_close(group);
    srt_close(listener);
    srt_cleanup();
    return 0;
}
