/*
 * Example: an event loop for AgniJS in C.
 *
 * The host provides `setTimeout(callback, ms)` and `console.log`, keeps the
 * timers in its own queue on a virtual clock, and pumps the engine's job
 * queue (promise reactions, async functions) between timers. Run it with no
 * arguments; it prints the order things happened in and exits 0 when that
 * order is the expected one.
 */
#include "agnijs.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define S(lit) lit, (sizeof lit - 1)

/* ---- the host's timer queue ---- */

typedef struct {
  double due;
  unsigned long seq; /* ties run in the order they were scheduled */
  AgnijsValue callback;
} Timer;

typedef struct {
  Timer items[64];
  size_t count;
  unsigned long next_seq;
  double now;
} Loop;

/* Everything printed, to compare with the expected order at the end. */
static char output[1024];

static void console_hook(void *user_data, AgnijsRealm realm, int32_t level, const char *text, size_t len) {
  (void)user_data;
  (void)realm;
  (void)level;
  printf("%.*s\n", (int)len, text);
  size_t used = strlen(output);
  if (used + len + 2 < sizeof output) {
    memcpy(output + used, text, len);
    output[used + len] = '|';
    output[used + len + 1] = 0;
  }
}

static double clock_hook(void *user_data) { return ((Loop *)user_data)->now; }

static AgnijsStatus throw_type_error(AgnijsEngine e, AgnijsRealm r, const char *msg, size_t len) {
  AgnijsValue err = 0;
  if (agnijs_new_error(e, r, AGNIJS_TYPE_ERROR, msg, len, &err) == AGNIJS_OK) {
    agnijs_throw(e, err);
    agnijs_value_free(e, err);
  }
  return AGNIJS_EXCEPTION;
}

/* setTimeout(callback, ms): the callback handle is duplicated, because the
 * argument handles are freed when this function returns. */
static AgnijsStatus set_timeout(AgnijsEngine e, const AgnijsCallInfo *call, void *user_data, AgnijsValue *result) {
  Loop *loop = (Loop *)user_data;
  int32_t type = 0;
  if (call->argc < 1 || agnijs_value_type(e, call->args[0], &type) != AGNIJS_OK || type != AGNIJS_TYPE_FUNCTION)
    return throw_type_error(e, call->realm, S("setTimeout needs a function"));
  double ms = 0;
  if (call->argc >= 2 && agnijs_value_get_number(e, call->args[1], &ms) != AGNIJS_OK) ms = 0;
  if (loop->count == sizeof loop->items / sizeof loop->items[0])
    return throw_type_error(e, call->realm, S("too many timers"));
  Timer *t = &loop->items[loop->count];
  if (agnijs_value_dup(e, call->args[0], &t->callback) != AGNIJS_OK) return AGNIJS_ERR_HOST;
  t->due = loop->now + (ms > 0 ? ms : 0);
  t->seq = loop->next_seq++;
  loop->count++;
  return agnijs_new_number(e, (double)t->seq, result);
}

/* Remove and return the earliest timer. */
static Timer pop_earliest(Loop *loop) {
  size_t best = 0;
  for (size_t i = 1; i < loop->count; i++) {
    const Timer *a = &loop->items[i], *b = &loop->items[best];
    if (a->due < b->due || (a->due == b->due && a->seq < b->seq)) best = i;
  }
  Timer t = loop->items[best];
  loop->items[best] = loop->items[loop->count - 1];
  loop->count--;
  return t;
}

static void report_exception(AgnijsEngine e) {
  AgnijsException ex;
  memset(&ex, 0, sizeof ex);
  ex.struct_size = (uint32_t)sizeof ex;
  if (agnijs_take_exception(e, &ex) != AGNIJS_OK) return;
  const uint8_t *p = NULL;
  size_t n = 0;
  if (agnijs_buffer_data(e, ex.message, &p, &n) == AGNIJS_OK) fprintf(stderr, "uncaught: %.*s\n", (int)n, (const char *)p);
  agnijs_buffer_free(e, ex.name);
  agnijs_buffer_free(e, ex.message);
  agnijs_buffer_free(e, ex.stack);
  agnijs_value_free(e, ex.value);
}

static const char script[] =
    "console.log('start');\n"
    "setTimeout(() => console.log('timeout 20'), 20);\n"
    "setTimeout(() => {\n"
    "  console.log('timeout 10');\n"
    "  Promise.resolve().then(() => console.log('microtask after 10'));\n"
    "}, 10);\n"
    "const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));\n"
    "(async () => {\n"
    "  await sleep(15);\n"
    "  console.log('slept 15 at ' + Date.now());\n"
    "})();\n"
    "Promise.resolve().then(() => console.log('microtask'));\n"
    "console.log('end of script');\n";

static const char expected[] =
    "start|end of script|microtask|timeout 10|microtask after 10|slept 15 at 15|timeout 20|";

int main(void) {
  Loop loop;
  memset(&loop, 0, sizeof loop);

  AgnijsEngineOptions o;
  memset(&o, 0, sizeof o);
  o.struct_size = (uint32_t)sizeof o;
  o.user_data = &loop;
  o.console = console_hook;
  o.now_ms = clock_hook;

  AgnijsEngine e = 0;
  if (agnijs_engine_new(&o, &e) != AGNIJS_OK) return 1;
  AgnijsRealm r = 0;
  agnijs_engine_main_realm(e, &r);

  /* globalThis.setTimeout = <host function>. */
  AgnijsValue global = 0, fn = 0;
  agnijs_global_object(e, r, &global);
  agnijs_new_function(e, r, S("setTimeout"), set_timeout, &loop, NULL, &fn);
  agnijs_set(e, r, global, S("setTimeout"), fn);
  agnijs_value_free(e, fn);
  agnijs_value_free(e, global);

  AgnijsValue result = 0;
  AgnijsStatus st = agnijs_eval_script(e, r, S("timers.js"), script, sizeof script - 1, &result);
  if (st == AGNIJS_OK) agnijs_value_free(e, result);
  else report_exception(e);

  /* The loop: drain the job queue, then run the earliest timer. */
  for (;;) {
    size_t ran = 0;
    if (agnijs_run_jobs(e, r, &ran) == AGNIJS_EXCEPTION) report_exception(e);
    if (loop.count == 0) break;
    Timer t = pop_earliest(&loop);
    if (t.due > loop.now) loop.now = t.due;
    AgnijsValue out = 0;
    st = agnijs_call(e, r, t.callback, 0, NULL, 0, &out);
    if (st == AGNIJS_OK) agnijs_value_free(e, out);
    else if (st == AGNIJS_EXCEPTION) report_exception(e);
    agnijs_value_free(e, t.callback);
  }

  agnijs_engine_free(e);
  if (strcmp(output, expected) != 0) {
    fprintf(stderr, "unexpected order:\n  got  %s\n  want %s\n", output, expected);
    return 1;
  }
  return 0;
}
