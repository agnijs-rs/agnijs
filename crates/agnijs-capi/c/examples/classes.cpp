// Example: AgniJS from C++, with a small RAII layer over the C API and a
// host class whose instances own C++ objects.
//
// JavaScript gets `new Counter(start)` (a host function used as a
// constructor) whose instances carry a C++ `Counter`; `increment()` and
// `value` live on a shared prototype. The engine calls the class finalizer
// when an instance is collected or the engine is freed, which deletes the
// C++ object. Exits 0 when every check passes.
//
// C++ exceptions must never unwind into the library: every callback below
// catches everything and reports it as a JavaScript error instead.
#include "agnijs.h"

#include <cstdio>
#include <cstring>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace agnijs {

// A failed call, carrying the library's status and message.
struct Error : std::runtime_error {
  AgnijsStatus status;
  Error(AgnijsStatus s, const std::string &what) : std::runtime_error(what), status(s) {}
};

inline std::string last_error() {
  char buf[512];
  if (agnijs_last_error(buf, sizeof buf, nullptr) != AGNIJS_OK) return "(message unavailable)";
  return buf;
}

inline void check(AgnijsStatus s, const char *what) {
  if (s != AGNIJS_OK) throw Error(s, std::string(what) + ": " + last_error());
}

// An owned value handle, freed when it goes out of scope.
class Value {
 public:
  Value() = default;
  Value(AgnijsEngine e, AgnijsValue v) : e_(e), v_(v) {}
  Value(const Value &) = delete;
  Value &operator=(const Value &) = delete;
  Value(Value &&o) noexcept : e_(o.e_), v_(std::exchange(o.v_, 0)) {}
  Value &operator=(Value &&o) noexcept {
    if (this != &o) {
      reset();
      e_ = o.e_;
      v_ = std::exchange(o.v_, 0);
    }
    return *this;
  }
  ~Value() { reset(); }

  AgnijsValue id() const { return v_; }
  // Give up ownership (for returning a result to the engine).
  AgnijsValue release() { return std::exchange(v_, 0); }

 private:
  void reset() {
    if (v_) agnijs_value_free(e_, v_);
    v_ = 0;
  }
  AgnijsEngine e_ = 0;
  AgnijsValue v_ = 0;
};

// An engine and its main realm, freed when it goes out of scope.
class Engine {
 public:
  Engine() {
    AgnijsEngineOptions o;
    std::memset(&o, 0, sizeof o);
    o.struct_size = static_cast<uint32_t>(sizeof o);
    check(agnijs_engine_new(&o, &e_), "engine_new");
    check(agnijs_engine_main_realm(e_, &r_), "main_realm");
  }
  Engine(const Engine &) = delete;
  Engine &operator=(const Engine &) = delete;
  ~Engine() { agnijs_engine_free(e_); }

  AgnijsEngine id() const { return e_; }
  AgnijsRealm realm() const { return r_; }

  Value wrap(AgnijsValue v) const { return Value(e_, v); }

  Value eval(const std::string &src) {
    AgnijsValue v = 0;
    AgnijsStatus s = agnijs_eval_script(e_, r_, "classes.js", 10, src.data(), src.size(), &v);
    if (s == AGNIJS_EXCEPTION) throw Error(s, "script threw: " + take_exception_message());
    check(s, "eval");
    return wrap(v);
  }

  Value global() {
    AgnijsValue v = 0;
    check(agnijs_global_object(e_, r_, &v), "global_object");
    return wrap(v);
  }

  Value get(const Value &obj, const std::string &key) {
    AgnijsValue v = 0;
    check(agnijs_get(e_, r_, obj.id(), key.data(), key.size(), &v), "get");
    return wrap(v);
  }

  void set(const Value &obj, const std::string &key, const Value &v) {
    check(agnijs_set(e_, r_, obj.id(), key.data(), key.size(), v.id()), "set");
  }

  std::string to_string(const Value &v) {
    AgnijsValue s = 0;
    check(agnijs_to_string(e_, r_, v.id(), &s), "to_string");
    Value owned = wrap(s);
    size_t needed = 0;
    agnijs_string_to_utf8(e_, owned.id(), nullptr, 0, &needed);
    std::vector<char> buf(needed + 1);
    check(agnijs_string_to_utf8(e_, owned.id(), buf.data(), buf.size(), &needed), "string_to_utf8");
    return std::string(buf.data(), needed);
  }

  std::string take_exception_message() {
    AgnijsException ex;
    std::memset(&ex, 0, sizeof ex);
    ex.struct_size = static_cast<uint32_t>(sizeof ex);
    if (agnijs_take_exception(e_, &ex) != AGNIJS_OK) return "(no exception)";
    const uint8_t *p = nullptr;
    size_t n = 0;
    std::string msg;
    if (agnijs_buffer_data(e_, ex.message, &p, &n) == AGNIJS_OK) msg.assign(reinterpret_cast<const char *>(p), n);
    agnijs_buffer_free(e_, ex.name);
    agnijs_buffer_free(e_, ex.message);
    agnijs_buffer_free(e_, ex.stack);
    agnijs_value_free(e_, ex.value);
    return msg;
  }

 private:
  AgnijsEngine e_ = 0;
  AgnijsRealm r_ = 0;
};

// Throw a JavaScript error of `kind` from a host function.
inline AgnijsStatus throw_js(AgnijsEngine e, AgnijsRealm r, int32_t kind, const std::string &msg) {
  AgnijsValue err = 0;
  if (agnijs_new_error(e, r, kind, msg.data(), msg.size(), &err) == AGNIJS_OK) {
    agnijs_throw(e, err);
    agnijs_value_free(e, err);
  }
  return AGNIJS_EXCEPTION;
}

}  // namespace agnijs

// ---- the host class ----

struct Counter {
  double count;
  explicit Counter(double start) : count(start) {}
};

static int live_counters = 0;

struct ClassState {
  AgnijsClass cls = 0;
  AgnijsValue prototype = 0;
};

static void counter_finalize(void *data) {
  delete static_cast<Counter *>(data);
  live_counters--;
}

// The Counter of `this`, or nullptr when `this` is not a Counter.
static Counter *this_counter(AgnijsEngine e, const AgnijsCallInfo *call, const ClassState *cs) {
  void *data = nullptr;
  if (agnijs_host_object_data(e, call->this_value, cs->cls, &data) != AGNIJS_OK) return nullptr;
  return static_cast<Counter *>(data);
}

// makeCounter(start): a new instance. Host functions are not constructors,
// so the JavaScript `Counter` below wraps this one.
static AgnijsStatus make_counter(AgnijsEngine e, const AgnijsCallInfo *call, void *user_data, AgnijsValue *result) {
  try {
    const ClassState *cs = static_cast<const ClassState *>(user_data);
    double start = 0;
    if (call->argc > 0 && agnijs_value_get_number(e, call->args[0], &start) != AGNIJS_OK)
      return agnijs::throw_js(e, call->realm, AGNIJS_TYPE_ERROR, "Counter(start) takes a number");
    Counter *c = new Counter(start);
    AgnijsStatus s = agnijs_new_host_object(e, call->realm, cs->cls, c, cs->prototype, result);
    if (s != AGNIJS_OK) {
      delete c;
      return s;
    }
    live_counters++;  // the engine owns `c` now; counter_finalize deletes it
    return AGNIJS_OK;
  } catch (const std::exception &ex) {
    return agnijs::throw_js(e, call->realm, AGNIJS_ERROR, ex.what());
  } catch (...) {
    return agnijs::throw_js(e, call->realm, AGNIJS_ERROR, "unknown C++ exception");
  }
}

static AgnijsStatus counter_increment(AgnijsEngine e, const AgnijsCallInfo *call, void *user_data, AgnijsValue *result) {
  try {
    Counter *c = this_counter(e, call, static_cast<const ClassState *>(user_data));
    if (!c) return agnijs::throw_js(e, call->realm, AGNIJS_TYPE_ERROR, "increment called on a non-Counter");
    double by = 1;
    if (call->argc > 0 && agnijs_value_get_number(e, call->args[0], &by) != AGNIJS_OK)
      throw std::invalid_argument("increment(by) takes a number");
    c->count += by;
    return agnijs_new_number(e, c->count, result);
  } catch (const std::exception &ex) {
    return agnijs::throw_js(e, call->realm, AGNIJS_TYPE_ERROR, ex.what());
  } catch (...) {
    return agnijs::throw_js(e, call->realm, AGNIJS_ERROR, "unknown C++ exception");
  }
}

static AgnijsStatus counter_value(AgnijsEngine e, const AgnijsCallInfo *call, void *user_data, AgnijsValue *result) {
  Counter *c = this_counter(e, call, static_cast<const ClassState *>(user_data));
  if (!c) return agnijs::throw_js(e, call->realm, AGNIJS_TYPE_ERROR, "value read from a non-Counter");
  return agnijs_new_number(e, c->count, result);
}

static int failures = 0;

static void expect(bool ok, const char *what) {
  if (!ok) {
    std::fprintf(stderr, "FAILED: %s\n", what);
    failures++;
  }
}

int main() {
  try {
    agnijs::Engine js;
    AgnijsEngine e = js.id();
    AgnijsRealm r = js.realm();
    ClassState cs;

    AgnijsClassDef def;
    std::memset(&def, 0, sizeof def);
    def.struct_size = static_cast<uint32_t>(sizeof def);
    def.name = "Counter";
    def.name_len = 7;
    def.finalize = counter_finalize;
    agnijs::check(agnijs_register_class(e, &def, &cs.cls), "register_class");

    // The shared prototype, with host methods on it.
    AgnijsValue proto = 0;
    agnijs::check(agnijs_new_object(e, r, &proto), "new_object");
    agnijs::Value prototype = js.wrap(proto);
    cs.prototype = prototype.id();
    {
      AgnijsValue f = 0;
      agnijs::check(agnijs_new_function(e, r, "increment", 9, counter_increment, &cs, nullptr, &f), "new_function");
      js.set(prototype, "increment", js.wrap(f));
      agnijs::check(agnijs_new_function(e, r, "value", 5, counter_value, &cs, nullptr, &f), "new_function");
      js.set(prototype, "valueOf", js.wrap(f));
      agnijs::check(agnijs_new_function(e, r, "makeCounter", 11, make_counter, &cs, nullptr, &f), "new_function");
      agnijs::Value global = js.global();
      js.set(global, "makeCounter", js.wrap(f));
      js.set(global, "counterPrototype", prototype);
    }
    // The constructor, in JavaScript, around the host factory.
    js.eval(
        "globalThis.Counter = ((make, proto) => {\n"
        "  function Counter(start) {\n"
        "    if (!new.target) throw new TypeError('Counter needs new');\n"
        "    return make(start);\n"
        "  }\n"
        "  Counter.prototype = proto;\n"
        "  proto.constructor = Counter;\n"
        "  return Counter;\n"
        "})(makeCounter, counterPrototype);\n"
        "delete globalThis.makeCounter; delete globalThis.counterPrototype;");

    agnijs::Value v = js.eval(
        "const c = new Counter(40);\n"
        "c.increment(); c.increment();\n"
        "const d = new Counter(1);\n"
        "[c.valueOf(), d.increment(10), c instanceof Object, String(+c)].join(' ')");
    std::string text = js.to_string(v);
    std::printf("%s\n", text.c_str());
    expect(text == "42 11 true 42", "counter results");
    expect(live_counters == 2, "two live counters");

    // Misuse from JavaScript becomes JavaScript errors, not crashes.
    agnijs::Value errs = js.eval(
        "const out = [];\n"
        "try { Counter(1) } catch (e) { out.push(e.message) }\n"
        "try { c.increment('x') } catch (e) { out.push(e.message) }\n"
        "try { Counter.prototype.increment.call({}) } catch (e) { out.push(e.message) }\n"
        "out.join('; ')");
    std::string msgs = js.to_string(errs);
    std::printf("%s\n", msgs.c_str());
    expect(msgs == "Counter needs new; increment(by) takes a number; increment called on a non-Counter", "error messages");

    // A C++ exception from the wrapper itself (a script that throws).
    try {
      js.eval("throw new RangeError('from script')");
      expect(false, "script exception reported");
    } catch (const agnijs::Error &err) {
      expect(err.status == AGNIJS_EXCEPTION && std::string(err.what()).find("from script") != std::string::npos,
             "script exception reported");
    }
  } catch (const std::exception &ex) {
    std::fprintf(stderr, "error: %s\n", ex.what());
    return 1;
  }
  // The engine is gone, so every instance has been finalized.
  expect(live_counters == 0, "every counter finalized");
  return failures == 0 ? 0 : 1;
}
