/*
 * The C test program for the AgniJS C API: the API used from real C, the
 * way an embedder would, plus misuse that must come back as error codes.
 * Exits 0 when every check passes. Built and run in CI against both the
 * static and the shared library, and under AddressSanitizer and
 * LeakSanitizer on Linux.
 */
#include "agnijs.h"

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;

#define CHECK(cond)                                                              \
  do {                                                                           \
    if (!(cond)) {                                                               \
      char msg_[512];                                                            \
      agnijs_last_error(msg_, sizeof msg_, NULL);                                \
      fprintf(stderr, "%s:%d: CHECK failed: %s (last error: %s)\n", __FILE__,   \
              __LINE__, #cond, msg_);                                            \
      failures++;                                                                \
    }                                                                            \
  } while (0)

#define S(lit) lit, (sizeof lit - 1)

static AgnijsEngineOptions options(void) {
  AgnijsEngineOptions o;
  memset(&o, 0, sizeof o);
  o.struct_size = (uint32_t)sizeof o;
  return o;
}

static AgnijsValue eval(AgnijsEngine e, AgnijsRealm r, const char *src) {
  AgnijsValue v = 0;
  AgnijsStatus st = agnijs_eval_script(e, r, S("test.c"), src, strlen(src), &v);
  CHECK(st == AGNIJS_OK);
  return v;
}

/* The text of a string value, in a malloc'd buffer the caller frees. */
static char *text(AgnijsEngine e, AgnijsValue v) {
  size_t needed = 0;
  AgnijsStatus st = agnijs_string_to_utf8(e, v, NULL, 0, &needed);
  CHECK(st == AGNIJS_ERR_BUFFER_TOO_SMALL || st == AGNIJS_OK);
  char *buf = (char *)malloc(needed + 1);
  if (!buf) abort();
  CHECK(agnijs_string_to_utf8(e, v, buf, needed + 1, &needed) == AGNIJS_OK);
  return buf;
}

static int text_is(AgnijsEngine e, AgnijsValue v, const char *want) {
  char *got = text(e, v);
  int same = strcmp(got, want) == 0;
  if (!same) fprintf(stderr, "  got \"%s\", want \"%s\"\n", got, want);
  free(got);
  return same;
}

static char *buffer_text(AgnijsEngine e, AgnijsBuffer b) {
  const uint8_t *p = NULL;
  size_t n = 0;
  CHECK(agnijs_buffer_data(e, b, &p, &n) == AGNIJS_OK);
  char *s = (char *)malloc(n + 1);
  if (!s) abort();
  if (n) memcpy(s, p, n);
  s[n] = 0;
  CHECK(agnijs_buffer_free(e, b) == AGNIJS_OK);
  return s;
}

/* ---- host function: sum(...numbers), throwing a TypeError otherwise ---- */

static int sum_calls = 0;
static int sum_finalized = 0;

static AgnijsStatus sum(AgnijsEngine e, const AgnijsCallInfo *call, void *user_data, AgnijsValue *result) {
  (void)user_data;
  sum_calls++;
  double total = 0;
  for (size_t i = 0; i < call->argc; i++) {
    double n;
    if (agnijs_value_get_number(e, call->args[i], &n) != AGNIJS_OK) {
      AgnijsValue err = 0;
      agnijs_new_error(e, call->realm, AGNIJS_TYPE_ERROR, S("sum takes numbers"), &err);
      agnijs_throw(e, err);
      agnijs_value_free(e, err);
      return AGNIJS_EXCEPTION;
    }
    total += n;
  }
  return agnijs_new_number(e, total, result);
}

static void sum_finalize(void *data) {
  (void)data;
  sum_finalized++;
}

/* ---- module hooks: an in-memory module map ---- */

static AgnijsStatus load(void *user_data, const char *name, size_t len, AgnijsSink sink) {
  (void)user_data;
  if (len == 7 && memcmp(name, "main.js", 7) == 0) {
    static const char src[] = "import { base } from 'dep.js'; export default base + 1;";
    return agnijs_sink_set_module(sink, AGNIJS_MODULE_SOURCE, (const uint8_t *)src, sizeof src - 1);
  }
  if (len == 6 && memcmp(name, "dep.js", 6) == 0) {
    static const char src[] = "export const base = 41;";
    return agnijs_sink_set_module(sink, AGNIJS_MODULE_SOURCE, (const uint8_t *)src, sizeof src - 1);
  }
  agnijs_sink_set_error(sink, S("no such module"));
  return AGNIJS_ERR_NOT_FOUND;
}

/* ---- host class ---- */

typedef struct {
  int x, y;
} Point;

static int points_freed = 0;

static void point_finalize(void *data) {
  free(data);
  points_freed++;
}

static void test_basics(void) {
  AgnijsEngineOptions o = options();
  o.load_module = load;
  AgnijsEngine e = 0;
  CHECK(agnijs_engine_new(&o, &e) == AGNIJS_OK);
  AgnijsRealm r = 0;
  CHECK(agnijs_engine_main_realm(e, &r) == AGNIJS_OK);

  /* Numbers, strings and conversions. */
  AgnijsValue v = eval(e, r, "6 * 7");
  double n = 0;
  CHECK(agnijs_value_get_number(e, v, &n) == AGNIJS_OK && n == 42);
  CHECK(agnijs_value_free(e, v) == AGNIJS_OK);
  v = eval(e, r, "'h\\u00e9llo'");
  CHECK(text_is(e, v, "h\xc3\xa9llo"));
  agnijs_value_free(e, v);

  /* A host function on the global object. */
  AgnijsValue f = 0, g = 0;
  CHECK(agnijs_new_function(e, r, S("sum"), sum, NULL, sum_finalize, &f) == AGNIJS_OK);
  CHECK(agnijs_global_object(e, r, &g) == AGNIJS_OK);
  CHECK(agnijs_set(e, r, g, S("sum"), f) == AGNIJS_OK);
  agnijs_value_free(e, f);
  v = eval(e, r, "String(sum(1, 2, 3))");
  CHECK(text_is(e, v, "6"));
  agnijs_value_free(e, v);
  v = eval(e, r, "try { sum('x') } catch (e) { e.name + ': ' + e.message }");
  CHECK(text_is(e, v, "TypeError: sum takes numbers"));
  agnijs_value_free(e, v);

  /* Calling a JavaScript function from C. */
  AgnijsValue fn = eval(e, r, "(function (a, b) { return a + b; })");
  AgnijsValue args[2];
  agnijs_new_number(e, 40, &args[0]);
  agnijs_new_number(e, 2, &args[1]);
  CHECK(agnijs_call(e, r, fn, 0, args, 2, &v) == AGNIJS_OK);
  CHECK(agnijs_value_get_number(e, v, &n) == AGNIJS_OK && n == 42);
  agnijs_value_free(e, v);
  agnijs_value_free(e, args[0]);
  agnijs_value_free(e, args[1]);
  agnijs_value_free(e, fn);

  /* Exceptions with name, message and stack. */
  static const char bad[] = "function boom() { throw new RangeError('far'); }\nboom();";
  CHECK(agnijs_eval_script(e, r, S("bad.js"), S(bad), &v) == AGNIJS_EXCEPTION);
  AgnijsException ex;
  memset(&ex, 0, sizeof ex);
  ex.struct_size = (uint32_t)sizeof ex;
  CHECK(agnijs_take_exception(e, &ex) == AGNIJS_OK);
  char *name = buffer_text(e, ex.name), *message = buffer_text(e, ex.message), *stack = buffer_text(e, ex.stack);
  CHECK(strcmp(name, "RangeError") == 0);
  CHECK(strcmp(message, "far") == 0);
  CHECK(strstr(stack, "boom (bad.js:1") != NULL);
  CHECK(ex.frame_count >= 1);
  free(name);
  free(message);
  free(stack);
  agnijs_value_free(e, ex.value);

  /* Promises and jobs. */
  AgnijsValue p = eval(e, r, "Promise.resolve(1).then(x => x * 10)");
  int32_t state = -1;
  CHECK(agnijs_run_jobs(e, r, NULL) == AGNIJS_OK);
  CHECK(agnijs_promise_state(e, r, p, &state, &v) == AGNIJS_OK && state == AGNIJS_PROMISE_FULFILLED);
  CHECK(agnijs_value_get_number(e, v, &n) == AGNIJS_OK && n == 10);
  agnijs_value_free(e, v);
  agnijs_value_free(e, p);

  /* Modules through the load hook. */
  CHECK(agnijs_load_module(e, r, S("main.js"), &p) == AGNIJS_OK);
  agnijs_run_jobs(e, r, NULL);
  CHECK(agnijs_promise_state(e, r, p, &state, &v) == AGNIJS_OK && state == AGNIJS_PROMISE_FULFILLED);
  AgnijsValue d = 0;
  CHECK(agnijs_get(e, r, v, S("default"), &d) == AGNIJS_OK);
  CHECK(agnijs_value_get_number(e, d, &n) == AGNIJS_OK && n == 42);
  agnijs_value_free(e, d);
  agnijs_value_free(e, v);
  agnijs_value_free(e, p);

  /* A host class whose instances own malloc'd memory. */
  static const char cls[] = "Point";
  AgnijsClassDef def;
  memset(&def, 0, sizeof def);
  def.struct_size = (uint32_t)sizeof def;
  def.name = cls;
  def.name_len = sizeof cls - 1;
  def.finalize = point_finalize;
  AgnijsClass pc = 0;
  CHECK(agnijs_register_class(e, &def, &pc) == AGNIJS_OK);
  for (int i = 0; i < 3; i++) {
    Point *pt = (Point *)malloc(sizeof *pt);
    if (!pt) abort();
    pt->x = i;
    pt->y = -i;
    AgnijsValue obj = 0;
    CHECK(agnijs_new_host_object(e, r, pc, pt, 0, &obj) == AGNIJS_OK);
    void *data = NULL;
    CHECK(agnijs_host_object_data(e, obj, pc, &data) == AGNIJS_OK && ((Point *)data)->x == i);
    if (i == 0) CHECK(agnijs_set(e, r, g, S("kept"), obj) == AGNIJS_OK);
    agnijs_value_free(e, obj);
  }
  agnijs_collect_garbage(e);
  agnijs_run_jobs(e, r, NULL);
  CHECK(points_freed == 2);

  /* Bytecode. */
  AgnijsBuffer bc = 0;
  CHECK(agnijs_compile_script(e, r, S("bc.js"), S("'from bytecode'"), &bc) == AGNIJS_OK);
  const uint8_t *bytes = NULL;
  size_t len = 0;
  CHECK(agnijs_buffer_data(e, bc, &bytes, &len) == AGNIJS_OK && len > 0);
  CHECK(agnijs_eval_bytecode(e, r, bytes, len, &v) == AGNIJS_OK);
  CHECK(text_is(e, v, "from bytecode"));
  agnijs_value_free(e, v);
  CHECK(agnijs_eval_bytecode(e, r, bytes, len / 2, &v) == AGNIJS_ERR_BYTECODE);
  CHECK(agnijs_buffer_free(e, bc) == AGNIJS_OK);

  agnijs_value_free(e, g);
  AgnijsMemoryUsage m;
  memset(&m, 0, sizeof m);
  m.struct_size = (uint32_t)sizeof m;
  CHECK(agnijs_memory_usage(e, &m) == AGNIJS_OK);
  CHECK(m.host_handles == 0); /* everything this test took was freed */
  CHECK(agnijs_engine_free(e) == AGNIJS_OK);
  CHECK(points_freed == 3);
  CHECK(sum_calls == 2 && sum_finalized == 1);
}

static void test_misuse(void) {
  AgnijsEngine e = 0;
  CHECK(agnijs_engine_new(NULL, &e) == AGNIJS_OK);
  AgnijsRealm r = 0;
  agnijs_engine_main_realm(e, &r);
  AgnijsValue v = 0;
  double n;

  CHECK(agnijs_engine_new(NULL, NULL) == AGNIJS_ERR_NULL_POINTER);
  CHECK(agnijs_eval_script(e, r, NULL, 0, NULL, 4, &v) == AGNIJS_ERR_NULL_POINTER);
  CHECK(agnijs_eval_script(e, r, NULL, 0, S("\xff"), &v) == AGNIJS_ERR_INVALID_UTF8);
  CHECK(agnijs_eval_script(e, 0, NULL, 0, S("1"), &v) == AGNIJS_ERR_INVALID_REALM);
  CHECK(agnijs_new_string_utf8(e, "x", SIZE_MAX, &v) == AGNIJS_ERR_INVALID_ARGUMENT);
  CHECK(agnijs_value_get_number(e, 0, &n) == AGNIJS_ERR_INVALID_HANDLE);
  CHECK(agnijs_value_get_number(e, 0x0123456789abcdefULL, &n) < 0);
  CHECK(agnijs_value_free(e, 42) < 0);

  v = eval(e, r, "({})");
  CHECK(agnijs_value_free(e, v) == AGNIJS_OK);
  CHECK(agnijs_value_free(e, v) == AGNIJS_ERR_INVALID_HANDLE); /* double free */
  CHECK(agnijs_value_get_number(e, v, &n) == AGNIJS_ERR_INVALID_HANDLE); /* use after free */

  AgnijsEngineOptions o = options();
  o.struct_size = 4;
  AgnijsEngine e2 = 0;
  CHECK(agnijs_engine_new(&o, &e2) == AGNIJS_ERR_INVALID_ARGUMENT);

  /* An uncatchable loop is interrupted by a request (here made before the
     run, so it fires at the first poll). */
  CHECK(agnijs_engine_request_interrupt(e) == AGNIJS_OK);
  CHECK(agnijs_eval_script(e, r, NULL, 0, S("for (;;) {}"), &v) == AGNIJS_INTERRUPTED);

  /* The memory limit. */
  CHECK(agnijs_set_memory_limit(e, 16u << 20) == AGNIJS_OK);
  CHECK(agnijs_eval_script(e, r, NULL, 0, S("let a = []; for (;;) a.push([1, 2, 3]);"), &v) == AGNIJS_OUT_OF_MEMORY);
  CHECK(agnijs_set_memory_limit(e, 0) == AGNIJS_OK);

  /* A deeply nested script fails cleanly instead of overflowing the stack. */
  size_t depth = 100000;
  char *deep = (char *)malloc(depth * 2 + 1);
  if (!deep) abort();
  memset(deep, '[', depth);
  memset(deep + depth, ']', depth);
  deep[depth * 2] = 0;
  AgnijsStatus st = agnijs_eval_script(e, r, NULL, 0, deep, depth * 2, &v);
  CHECK(st == AGNIJS_EXCEPTION);
  if (st == AGNIJS_EXCEPTION) {
    AgnijsException ex;
    memset(&ex, 0, sizeof ex);
    ex.struct_size = (uint32_t)sizeof ex;
    agnijs_take_exception(e, &ex);
    agnijs_value_free(e, ex.value);
    agnijs_buffer_free(e, ex.name);
    agnijs_buffer_free(e, ex.message);
    agnijs_buffer_free(e, ex.stack);
  } else if (st == AGNIJS_OK) {
    agnijs_value_free(e, v);
  }
  free(deep);

  CHECK(agnijs_engine_free(e) == AGNIJS_OK);
  CHECK(agnijs_engine_free(e) == AGNIJS_ERR_INVALID_ENGINE);
  CHECK(agnijs_eval_script(e, r, NULL, 0, S("1"), &v) == AGNIJS_ERR_INVALID_ENGINE);
  (void)n;
}

int main(void) {
  if ((agnijs_abi_version() >> 16) != AGNIJS_ABI_VERSION_MAJOR) {
    fprintf(stderr, "ABI mismatch: header %d, library %u\n", AGNIJS_ABI_VERSION_MAJOR, agnijs_abi_version() >> 16);
    return 2;
  }
  printf("agnijs %s\n", agnijs_version_string());
  test_basics();
  test_misuse();
  if (failures) {
    fprintf(stderr, "%d check(s) failed\n", failures);
    return 1;
  }
  printf("all C API checks passed\n");
  return 0;
}
