/* A TCP echo server and client in one libuv event loop. This drives the
   per-OS source selection: uv__platform_loop_init lives in kqueue.c on
   macOS and linux.c on Linux, so a wrong `[[targets.uv.when]]` block is a
   link failure, and a wrong *runtime* answer shows up as the loop never
   completing. */
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include "uv.h"

#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            printf("FAIL: %s (line %d)\n", #cond, __LINE__);                   \
            exit(1);                                                           \
        }                                                                      \
    } while (0)

static const char MSG[] = "harbour-echo";

static uv_loop_t *loop;
static uv_tcp_t server;
static uv_tcp_t client;
static uv_tcp_t incoming;
static char echoed[64];
static int got_echo = 0;

static void alloc_cb(uv_handle_t *h, size_t sz, uv_buf_t *buf) {
    (void) h;
    buf->base = malloc(sz);
    buf->len = sz;
}

static void on_client_read(uv_stream_t *s, ssize_t nread, const uv_buf_t *buf) {
    if (nread > 0) {
        memcpy(echoed, buf->base, (size_t) nread);
        echoed[nread] = '\0';
        got_echo = 1;
        uv_close((uv_handle_t *) s, NULL);
        uv_close((uv_handle_t *) &incoming, NULL);
        uv_close((uv_handle_t *) &server, NULL);
    }
    free(buf->base);
}

static void on_server_write(uv_write_t *req, int status) {
    CHECK(status == 0);
    free(req);
}

static void on_server_read(uv_stream_t *s, ssize_t nread, const uv_buf_t *buf) {
    if (nread > 0) {
        uv_write_t *req = malloc(sizeof(uv_write_t));
        uv_buf_t out = uv_buf_init(buf->base, (unsigned int) nread);
        CHECK(uv_write(req, s, &out, 1, on_server_write) == 0);
    } else {
        free(buf->base);
    }
}

static void on_connection(uv_stream_t *srv, int status) {
    CHECK(status == 0);
    CHECK(uv_tcp_init(loop, &incoming) == 0);
    CHECK(uv_accept(srv, (uv_stream_t *) &incoming) == 0);
    CHECK(uv_read_start((uv_stream_t *) &incoming, alloc_cb, on_server_read) == 0);
}

static void on_connect(uv_connect_t *req, int status) {
    CHECK(status == 0);
    uv_write_t *w = malloc(sizeof(uv_write_t));
    uv_buf_t out = uv_buf_init((char *) MSG, (unsigned int) strlen(MSG));
    CHECK(uv_write(w, req->handle, &out, 1, on_server_write) == 0);
    CHECK(uv_read_start(req->handle, alloc_cb, on_client_read) == 0);
}

int main(void) {
    loop = uv_default_loop();
    CHECK(loop != NULL);

    struct sockaddr_in addr;
    CHECK(uv_ip4_addr("127.0.0.1", 0, &addr) == 0);
    CHECK(uv_tcp_init(loop, &server) == 0);
    CHECK(uv_tcp_bind(&server, (const struct sockaddr *) &addr, 0) == 0);

    /* Port 0 means the kernel chose one; read it back. */
    struct sockaddr_in bound;
    int len = sizeof(bound);
    CHECK(uv_tcp_getsockname(&server, (struct sockaddr *) &bound, &len) == 0);
    CHECK(uv_listen((uv_stream_t *) &server, 1, on_connection) == 0);

    CHECK(uv_tcp_init(loop, &client) == 0);
    static uv_connect_t creq;
    CHECK(uv_tcp_connect(&creq, &client, (const struct sockaddr *) &bound,
                         on_connect) == 0);

    CHECK(uv_run(loop, UV_RUN_DEFAULT) == 0);
    CHECK(got_echo == 1);
    CHECK(strcmp(echoed, MSG) == 0);

    /* uv_dlopen wraps dlopen -- the reason `dl` is on libuv's public link
       surface on Linux. */
    uv_lib_t lib;
    (void) uv_dlopen("definitely-not-a-real-library.so", &lib);

    printf("OK libuv %s: per-OS event loop, TCP echo round trip, uv_dlopen\n",
           uv_version_string());
    return 0;
}
