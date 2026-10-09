/**
 * Vendored @xterm/addon-webgl 0.19.0 (upstream commit f447274f430fd22513f6adbf9862d19524471c04) built against @xterm/xterm 6.0.0.
 * Source: vendor/xterm-addon-webgl/src — see vendor/xterm-addon-webgl/README.md for the applied fixes.
 * Regenerate with `node scripts/build-webgl-addon.mjs`. MIT License — Copyright (c) The xterm.js authors.
 */
var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __decorateClass = (decorators, target, key, kind) => {
  var result = kind > 1 ? void 0 : kind ? __getOwnPropDesc(target, key) : target;
  for (var i = decorators.length - 1, decorator; i >= 0; i--)
    if (decorator = decorators[i])
      result = (kind ? decorator(target, key, result) : decorator(result)) || result;
  if (kind && result) __defProp(target, key, result);
  return result;
};
var __decorateParam = (index, decorator) => (target, key) => decorator(target, key, index);

// node_modules/@xterm/xterm/src/vs/base/common/errors.ts
var ErrorHandler = class {
  constructor() {
    this.listeners = [];
    this.unexpectedErrorHandler = function(e) {
      setTimeout(() => {
        if (e.stack) {
          if (ErrorNoTelemetry.isErrorNoTelemetry(e)) {
            throw new ErrorNoTelemetry(e.message + "\n\n" + e.stack);
          }
          throw new Error(e.message + "\n\n" + e.stack);
        }
        throw e;
      }, 0);
    };
  }
  addListener(listener) {
    this.listeners.push(listener);
    return () => {
      this._removeListener(listener);
    };
  }
  emit(e) {
    this.listeners.forEach((listener) => {
      listener(e);
    });
  }
  _removeListener(listener) {
    this.listeners.splice(this.listeners.indexOf(listener), 1);
  }
  setUnexpectedErrorHandler(newUnexpectedErrorHandler) {
    this.unexpectedErrorHandler = newUnexpectedErrorHandler;
  }
  getUnexpectedErrorHandler() {
    return this.unexpectedErrorHandler;
  }
  onUnexpectedError(e) {
    this.unexpectedErrorHandler(e);
    this.emit(e);
  }
  // For external errors, we don't want the listeners to be called
  onUnexpectedExternalError(e) {
    this.unexpectedErrorHandler(e);
  }
};
var errorHandler = new ErrorHandler();
function onUnexpectedError(e) {
  if (!isCancellationError(e)) {
    errorHandler.onUnexpectedError(e);
  }
  return void 0;
}
var canceledName = "Canceled";
function isCancellationError(error) {
  if (error instanceof CancellationError) {
    return true;
  }
  return error instanceof Error && error.name === canceledName && error.message === canceledName;
}
var CancellationError = class extends Error {
  constructor() {
    super(canceledName);
    this.name = this.message;
  }
};
var ErrorNoTelemetry = class _ErrorNoTelemetry extends Error {
  constructor(msg) {
    super(msg);
    this.name = "CodeExpectedError";
  }
  static fromError(err) {
    if (err instanceof _ErrorNoTelemetry) {
      return err;
    }
    const result = new _ErrorNoTelemetry();
    result.message = err.message;
    result.stack = err.stack;
    return result;
  }
  static isErrorNoTelemetry(err) {
    return err.name === "CodeExpectedError";
  }
};

// node_modules/@xterm/xterm/src/vs/base/common/arraysFind.ts
function findLastIdxMonotonous(array, predicate, startIdx = 0, endIdxEx = array.length) {
  let i = startIdx;
  let j = endIdxEx;
  while (i < j) {
    const k = Math.floor((i + j) / 2);
    if (predicate(array[k])) {
      i = k + 1;
    } else {
      j = k;
    }
  }
  return i - 1;
}
var _MonotonousArray = class _MonotonousArray {
  constructor(_array) {
    this._array = _array;
    this._findLastMonotonousLastIdx = 0;
  }
  /**
   * The predicate must be monotonous, i.e. `arr.map(predicate)` must be like `[true, ..., true, false, ..., false]`!
   * For subsequent calls, current predicate must be weaker than (or equal to) the previous predicate, i.e. more entries must be `true`.
   */
  findLastMonotonous(predicate) {
    if (_MonotonousArray.assertInvariants) {
      if (this._prevFindLastPredicate) {
        for (const item of this._array) {
          if (this._prevFindLastPredicate(item) && !predicate(item)) {
            throw new Error("MonotonousArray: current predicate must be weaker than (or equal to) the previous predicate.");
          }
        }
      }
      this._prevFindLastPredicate = predicate;
    }
    const idx = findLastIdxMonotonous(this._array, predicate, this._findLastMonotonousLastIdx);
    this._findLastMonotonousLastIdx = idx + 1;
    return idx === -1 ? void 0 : this._array[idx];
  }
};
_MonotonousArray.assertInvariants = false;
var MonotonousArray = _MonotonousArray;

// node_modules/@xterm/xterm/src/vs/base/common/arrays.ts
var CompareResult;
((CompareResult2) => {
  function isLessThan(result) {
    return result < 0;
  }
  CompareResult2.isLessThan = isLessThan;
  function isLessThanOrEqual(result) {
    return result <= 0;
  }
  CompareResult2.isLessThanOrEqual = isLessThanOrEqual;
  function isGreaterThan(result) {
    return result > 0;
  }
  CompareResult2.isGreaterThan = isGreaterThan;
  function isNeitherLessOrGreaterThan(result) {
    return result === 0;
  }
  CompareResult2.isNeitherLessOrGreaterThan = isNeitherLessOrGreaterThan;
  CompareResult2.greaterThan = 1;
  CompareResult2.lessThan = -1;
  CompareResult2.neitherLessOrGreaterThan = 0;
})(CompareResult || (CompareResult = {}));
function compareBy(selector, comparator) {
  return (a, b) => comparator(selector(a), selector(b));
}
var numberComparator = (a, b) => a - b;
var _CallbackIterable = class _CallbackIterable {
  constructor(iterate) {
    this.iterate = iterate;
  }
  forEach(handler) {
    this.iterate((item) => {
      handler(item);
      return true;
    });
  }
  toArray() {
    const result = [];
    this.iterate((item) => {
      result.push(item);
      return true;
    });
    return result;
  }
  filter(predicate) {
    return new _CallbackIterable((cb) => this.iterate((item) => predicate(item) ? cb(item) : true));
  }
  map(mapFn) {
    return new _CallbackIterable((cb) => this.iterate((item) => cb(mapFn(item))));
  }
  some(predicate) {
    let result = false;
    this.iterate((item) => {
      result = predicate(item);
      return !result;
    });
    return result;
  }
  findFirst(predicate) {
    let result;
    this.iterate((item) => {
      if (predicate(item)) {
        result = item;
        return false;
      }
      return true;
    });
    return result;
  }
  findLast(predicate) {
    let result;
    this.iterate((item) => {
      if (predicate(item)) {
        result = item;
      }
      return true;
    });
    return result;
  }
  findLastMaxBy(comparator) {
    let result;
    let first = true;
    this.iterate((item) => {
      if (first || CompareResult.isGreaterThan(comparator(item, result))) {
        first = false;
        result = item;
      }
      return true;
    });
    return result;
  }
};
_CallbackIterable.empty = new _CallbackIterable((_callback) => {
});
var CallbackIterable = _CallbackIterable;

// node_modules/@xterm/xterm/src/vs/base/common/collections.ts
function groupBy(data, groupFn) {
  const result = /* @__PURE__ */ Object.create(null);
  for (const element of data) {
    const key = groupFn(element);
    let target = result[key];
    if (!target) {
      target = result[key] = [];
    }
    target.push(element);
  }
  return result;
}
var _a, _b;
var SetWithKey = class {
  constructor(values, toKey) {
    this.toKey = toKey;
    this._map = /* @__PURE__ */ new Map();
    this[_a] = "SetWithKey";
    for (const value of values) {
      this.add(value);
    }
  }
  get size() {
    return this._map.size;
  }
  add(value) {
    const key = this.toKey(value);
    this._map.set(key, value);
    return this;
  }
  delete(value) {
    return this._map.delete(this.toKey(value));
  }
  has(value) {
    return this._map.has(this.toKey(value));
  }
  *entries() {
    for (const entry of this._map.values()) {
      yield [entry, entry];
    }
  }
  keys() {
    return this.values();
  }
  *values() {
    for (const entry of this._map.values()) {
      yield entry;
    }
  }
  clear() {
    this._map.clear();
  }
  forEach(callbackfn, thisArg) {
    this._map.forEach((entry) => callbackfn.call(thisArg, entry, entry, this));
  }
  [(_b = Symbol.iterator, _a = Symbol.toStringTag, _b)]() {
    return this.values();
  }
};

// node_modules/@xterm/xterm/src/vs/base/common/map.ts
var SetMap = class {
  constructor() {
    this.map = /* @__PURE__ */ new Map();
  }
  add(key, value) {
    let values = this.map.get(key);
    if (!values) {
      values = /* @__PURE__ */ new Set();
      this.map.set(key, values);
    }
    values.add(value);
  }
  delete(key, value) {
    const values = this.map.get(key);
    if (!values) {
      return;
    }
    values.delete(value);
    if (values.size === 0) {
      this.map.delete(key);
    }
  }
  forEach(key, fn) {
    const values = this.map.get(key);
    if (!values) {
      return;
    }
    values.forEach(fn);
  }
  get(key) {
    const values = this.map.get(key);
    if (!values) {
      return /* @__PURE__ */ new Set();
    }
    return values;
  }
};

// node_modules/@xterm/xterm/src/vs/base/common/functional.ts
function createSingleCallFunction(fn, fnDidRunCallback) {
  const _this = this;
  let didCall = false;
  let result;
  return function() {
    if (didCall) {
      return result;
    }
    didCall = true;
    if (fnDidRunCallback) {
      try {
        result = fn.apply(_this, arguments);
      } finally {
        fnDidRunCallback();
      }
    } else {
      result = fn.apply(_this, arguments);
    }
    return result;
  };
}

// node_modules/@xterm/xterm/src/vs/base/common/iterator.ts
var Iterable;
((Iterable2) => {
  function is(thing) {
    return thing && typeof thing === "object" && typeof thing[Symbol.iterator] === "function";
  }
  Iterable2.is = is;
  const _empty = Object.freeze([]);
  function empty() {
    return _empty;
  }
  Iterable2.empty = empty;
  function* single(element) {
    yield element;
  }
  Iterable2.single = single;
  function wrap(iterableOrElement) {
    if (is(iterableOrElement)) {
      return iterableOrElement;
    } else {
      return single(iterableOrElement);
    }
  }
  Iterable2.wrap = wrap;
  function from(iterable) {
    return iterable || _empty;
  }
  Iterable2.from = from;
  function* reverse(array) {
    for (let i = array.length - 1; i >= 0; i--) {
      yield array[i];
    }
  }
  Iterable2.reverse = reverse;
  function isEmpty(iterable) {
    return !iterable || iterable[Symbol.iterator]().next().done === true;
  }
  Iterable2.isEmpty = isEmpty;
  function first(iterable) {
    return iterable[Symbol.iterator]().next().value;
  }
  Iterable2.first = first;
  function some(iterable, predicate) {
    let i = 0;
    for (const element of iterable) {
      if (predicate(element, i++)) {
        return true;
      }
    }
    return false;
  }
  Iterable2.some = some;
  function find(iterable, predicate) {
    for (const element of iterable) {
      if (predicate(element)) {
        return element;
      }
    }
    return void 0;
  }
  Iterable2.find = find;
  function* filter(iterable, predicate) {
    for (const element of iterable) {
      if (predicate(element)) {
        yield element;
      }
    }
  }
  Iterable2.filter = filter;
  function* map(iterable, fn) {
    let index = 0;
    for (const element of iterable) {
      yield fn(element, index++);
    }
  }
  Iterable2.map = map;
  function* flatMap(iterable, fn) {
    let index = 0;
    for (const element of iterable) {
      yield* fn(element, index++);
    }
  }
  Iterable2.flatMap = flatMap;
  function* concat(...iterables) {
    for (const iterable of iterables) {
      yield* iterable;
    }
  }
  Iterable2.concat = concat;
  function reduce(iterable, reducer, initialValue) {
    let value = initialValue;
    for (const element of iterable) {
      value = reducer(value, element);
    }
    return value;
  }
  Iterable2.reduce = reduce;
  function* slice(arr, from2, to = arr.length) {
    if (from2 < 0) {
      from2 += arr.length;
    }
    if (to < 0) {
      to += arr.length;
    } else if (to > arr.length) {
      to = arr.length;
    }
    for (; from2 < to; from2++) {
      yield arr[from2];
    }
  }
  Iterable2.slice = slice;
  function consume(iterable, atMost = Number.POSITIVE_INFINITY) {
    const consumed = [];
    if (atMost === 0) {
      return [consumed, iterable];
    }
    const iterator = iterable[Symbol.iterator]();
    for (let i = 0; i < atMost; i++) {
      const next = iterator.next();
      if (next.done) {
        return [consumed, Iterable2.empty()];
      }
      consumed.push(next.value);
    }
    return [consumed, { [Symbol.iterator]() {
      return iterator;
    } }];
  }
  Iterable2.consume = consume;
  async function asyncToArray(iterable) {
    const result = [];
    for await (const item of iterable) {
      result.push(item);
    }
    return Promise.resolve(result);
  }
  Iterable2.asyncToArray = asyncToArray;
})(Iterable || (Iterable = {}));

// node_modules/@xterm/xterm/src/vs/base/common/lifecycle.ts
var TRACK_DISPOSABLES = false;
var disposableTracker = null;
var _DisposableTracker = class _DisposableTracker {
  constructor() {
    this.livingDisposables = /* @__PURE__ */ new Map();
  }
  getDisposableData(d) {
    let val = this.livingDisposables.get(d);
    if (!val) {
      val = { parent: null, source: null, isSingleton: false, value: d, idx: _DisposableTracker.idx++ };
      this.livingDisposables.set(d, val);
    }
    return val;
  }
  trackDisposable(d) {
    const data = this.getDisposableData(d);
    if (!data.source) {
      data.source = new Error().stack;
    }
  }
  setParent(child, parent) {
    const data = this.getDisposableData(child);
    data.parent = parent;
  }
  markAsDisposed(x) {
    this.livingDisposables.delete(x);
  }
  markAsSingleton(disposable) {
    this.getDisposableData(disposable).isSingleton = true;
  }
  getRootParent(data, cache) {
    const cacheValue = cache.get(data);
    if (cacheValue) {
      return cacheValue;
    }
    const result = data.parent ? this.getRootParent(this.getDisposableData(data.parent), cache) : data;
    cache.set(data, result);
    return result;
  }
  getTrackedDisposables() {
    const rootParentCache = /* @__PURE__ */ new Map();
    const leaking = [...this.livingDisposables.entries()].filter(([, v]) => v.source !== null && !this.getRootParent(v, rootParentCache).isSingleton).flatMap(([k]) => k);
    return leaking;
  }
  computeLeakingDisposables(maxReported = 10, preComputedLeaks) {
    let uncoveredLeakingObjs;
    if (preComputedLeaks) {
      uncoveredLeakingObjs = preComputedLeaks;
    } else {
      const rootParentCache = /* @__PURE__ */ new Map();
      const leakingObjects = [...this.livingDisposables.values()].filter((info) => info.source !== null && !this.getRootParent(info, rootParentCache).isSingleton);
      if (leakingObjects.length === 0) {
        return;
      }
      const leakingObjsSet = new Set(leakingObjects.map((o) => o.value));
      uncoveredLeakingObjs = leakingObjects.filter((l) => {
        return !(l.parent && leakingObjsSet.has(l.parent));
      });
      if (uncoveredLeakingObjs.length === 0) {
        throw new Error("There are cyclic diposable chains!");
      }
    }
    if (!uncoveredLeakingObjs) {
      return void 0;
    }
    function getStackTracePath(leaking) {
      function removePrefix(array, linesToRemove) {
        while (array.length > 0 && linesToRemove.some((regexp) => typeof regexp === "string" ? regexp === array[0] : array[0].match(regexp))) {
          array.shift();
        }
      }
      const lines = leaking.source.split("\n").map((p) => p.trim().replace("at ", "")).filter((l) => l !== "");
      removePrefix(lines, ["Error", /^trackDisposable \(.*\)$/, /^DisposableTracker.trackDisposable \(.*\)$/]);
      return lines.reverse();
    }
    const stackTraceStarts = new SetMap();
    for (const leaking of uncoveredLeakingObjs) {
      const stackTracePath = getStackTracePath(leaking);
      for (let i2 = 0; i2 <= stackTracePath.length; i2++) {
        stackTraceStarts.add(stackTracePath.slice(0, i2).join("\n"), leaking);
      }
    }
    uncoveredLeakingObjs.sort(compareBy((l) => l.idx, numberComparator));
    let message = "";
    let i = 0;
    for (const leaking of uncoveredLeakingObjs.slice(0, maxReported)) {
      i++;
      const stackTracePath = getStackTracePath(leaking);
      const stackTraceFormattedLines = [];
      for (let i2 = 0; i2 < stackTracePath.length; i2++) {
        let line = stackTracePath[i2];
        const starts = stackTraceStarts.get(stackTracePath.slice(0, i2 + 1).join("\n"));
        line = `(shared with ${starts.size}/${uncoveredLeakingObjs.length} leaks) at ${line}`;
        const prevStarts = stackTraceStarts.get(stackTracePath.slice(0, i2).join("\n"));
        const continuations = groupBy([...prevStarts].map((d) => getStackTracePath(d)[i2]), (v) => v);
        delete continuations[stackTracePath[i2]];
        for (const [cont, set] of Object.entries(continuations)) {
          stackTraceFormattedLines.unshift(`    - stacktraces of ${set.length} other leaks continue with ${cont}`);
        }
        stackTraceFormattedLines.unshift(line);
      }
      message += `


==================== Leaking disposable ${i}/${uncoveredLeakingObjs.length}: ${leaking.value.constructor.name} ====================
${stackTraceFormattedLines.join("\n")}
============================================================

`;
    }
    if (uncoveredLeakingObjs.length > maxReported) {
      message += `


... and ${uncoveredLeakingObjs.length - maxReported} more leaking disposables

`;
    }
    return { leaks: uncoveredLeakingObjs, details: message };
  }
};
_DisposableTracker.idx = 0;
var DisposableTracker = _DisposableTracker;
function setDisposableTracker(tracker) {
  disposableTracker = tracker;
}
if (TRACK_DISPOSABLES) {
  const __is_disposable_tracked__ = "__is_disposable_tracked__";
  setDisposableTracker(new class {
    trackDisposable(x) {
      const stack = new Error("Potentially leaked disposable").stack;
      setTimeout(() => {
        if (!x[__is_disposable_tracked__]) {
          console.log(stack);
        }
      }, 3e3);
    }
    setParent(child, parent) {
      if (child && child !== Disposable.None) {
        try {
          child[__is_disposable_tracked__] = true;
        } catch {
        }
      }
    }
    markAsDisposed(disposable) {
      if (disposable && disposable !== Disposable.None) {
        try {
          disposable[__is_disposable_tracked__] = true;
        } catch {
        }
      }
    }
    markAsSingleton(disposable) {
    }
  }());
}
function trackDisposable(x) {
  disposableTracker?.trackDisposable(x);
  return x;
}
function markAsDisposed(disposable) {
  disposableTracker?.markAsDisposed(disposable);
}
function setParentOfDisposable(child, parent) {
  disposableTracker?.setParent(child, parent);
}
function setParentOfDisposables(children, parent) {
  if (!disposableTracker) {
    return;
  }
  for (const child of children) {
    disposableTracker.setParent(child, parent);
  }
}
function dispose(arg) {
  if (Iterable.is(arg)) {
    const errors = [];
    for (const d of arg) {
      if (d) {
        try {
          d.dispose();
        } catch (e) {
          errors.push(e);
        }
      }
    }
    if (errors.length === 1) {
      throw errors[0];
    } else if (errors.length > 1) {
      throw new AggregateError(errors, "Encountered errors while disposing of store");
    }
    return Array.isArray(arg) ? [] : arg;
  } else if (arg) {
    arg.dispose();
    return arg;
  }
}
function combinedDisposable(...disposables) {
  const parent = toDisposable(() => dispose(disposables));
  setParentOfDisposables(disposables, parent);
  return parent;
}
function toDisposable(fn) {
  const self = trackDisposable({
    dispose: createSingleCallFunction(() => {
      markAsDisposed(self);
      fn();
    })
  });
  return self;
}
var _DisposableStore = class _DisposableStore {
  constructor() {
    this._toDispose = /* @__PURE__ */ new Set();
    this._isDisposed = false;
    trackDisposable(this);
  }
  /**
   * Dispose of all registered disposables and mark this object as disposed.
   *
   * Any future disposables added to this object will be disposed of on `add`.
   */
  dispose() {
    if (this._isDisposed) {
      return;
    }
    markAsDisposed(this);
    this._isDisposed = true;
    this.clear();
  }
  /**
   * @return `true` if this object has been disposed of.
   */
  get isDisposed() {
    return this._isDisposed;
  }
  /**
   * Dispose of all registered disposables but do not mark this object as disposed.
   */
  clear() {
    if (this._toDispose.size === 0) {
      return;
    }
    try {
      dispose(this._toDispose);
    } finally {
      this._toDispose.clear();
    }
  }
  /**
   * Add a new {@link IDisposable disposable} to the collection.
   */
  add(o) {
    if (!o) {
      return o;
    }
    if (o === this) {
      throw new Error("Cannot register a disposable on itself!");
    }
    setParentOfDisposable(o, this);
    if (this._isDisposed) {
      if (!_DisposableStore.DISABLE_DISPOSED_WARNING) {
        console.warn(new Error("Trying to add a disposable to a DisposableStore that has already been disposed of. The added object will be leaked!").stack);
      }
    } else {
      this._toDispose.add(o);
    }
    return o;
  }
  /**
   * Deletes a disposable from store and disposes of it. This will not throw or warn and proceed to dispose the
   * disposable even when the disposable is not part in the store.
   */
  delete(o) {
    if (!o) {
      return;
    }
    if (o === this) {
      throw new Error("Cannot dispose a disposable on itself!");
    }
    this._toDispose.delete(o);
    o.dispose();
  }
  /**
   * Deletes the value from the store, but does not dispose it.
   */
  deleteAndLeak(o) {
    if (!o) {
      return;
    }
    if (this._toDispose.has(o)) {
      this._toDispose.delete(o);
      setParentOfDisposable(o, null);
    }
  }
};
_DisposableStore.DISABLE_DISPOSED_WARNING = false;
var DisposableStore = _DisposableStore;
var Disposable = class {
  constructor() {
    this._store = new DisposableStore();
    trackDisposable(this);
    setParentOfDisposable(this._store, this);
  }
  dispose() {
    markAsDisposed(this);
    this._store.dispose();
  }
  /**
   * Adds `o` to the collection of disposables managed by this object.
   */
  _register(o) {
    if (o === this) {
      throw new Error("Cannot register a disposable on itself!");
    }
    return this._store.add(o);
  }
};
/**
 * A disposable that does nothing when it is disposed of.
 *
 * TODO: This should not be a static property.
 */
Disposable.None = Object.freeze({ dispose() {
} });
var MutableDisposable = class {
  constructor() {
    this._isDisposed = false;
    trackDisposable(this);
  }
  get value() {
    return this._isDisposed ? void 0 : this._value;
  }
  set value(value) {
    if (this._isDisposed || value === this._value) {
      return;
    }
    this._value?.dispose();
    if (value) {
      setParentOfDisposable(value, this);
    }
    this._value = value;
  }
  /**
   * Resets the stored value and disposed of the previously stored value.
   */
  clear() {
    this.value = void 0;
  }
  dispose() {
    this._isDisposed = true;
    markAsDisposed(this);
    this._value?.dispose();
    this._value = void 0;
  }
  /**
   * Clears the value, but does not dispose it.
   * The old value is returned.
  */
  clearAndLeak() {
    const oldValue = this._value;
    this._value = void 0;
    if (oldValue) {
      setParentOfDisposable(oldValue, null);
    }
    return oldValue;
  }
};

// node_modules/@xterm/xterm/src/common/Platform.ts
var isNode = typeof process !== "undefined" && "title" in process ? true : false;
var userAgent = isNode ? "node" : navigator.userAgent;
var platform = isNode ? "node" : navigator.platform;
var isFirefox = userAgent.includes("Firefox");
var isLegacyEdge = userAgent.includes("Edge");
var isSafari = /^((?!chrome|android).)*safari/i.test(userAgent);
function getSafariVersion() {
  if (!isSafari) {
    return 0;
  }
  const majorVersion = userAgent.match(/Version\/(\d+)/);
  if (majorVersion === null || majorVersion.length < 2) {
    return 0;
  }
  return parseInt(majorVersion[1]);
}
var isMac = ["Macintosh", "MacIntel", "MacPPC", "Mac68K"].includes(platform);
var isWindows = ["Windows", "Win16", "Win32", "WinCE"].includes(platform);
var isLinux = platform.indexOf("Linux") >= 0;
var isChromeOS = /\bCrOS\b/.test(userAgent);

// node_modules/@xterm/xterm/src/common/buffer/Constants.ts
var DEFAULT_COLOR = 0;
var DEFAULT_ATTR = 0 << 18 | DEFAULT_COLOR << 9 | 256 << 0;
var DEFAULT_EXT = 0;
var CHAR_DATA_ATTR_INDEX = 0;
var CHAR_DATA_CHAR_INDEX = 1;
var CHAR_DATA_WIDTH_INDEX = 2;
var NULL_CELL_CHAR = "";
var NULL_CELL_CODE = 0;

// node_modules/@xterm/xterm/src/common/Color.ts
var $r = 0;
var $g = 0;
var $b = 0;
var $a = 0;
var NULL_COLOR = {
  css: "#00000000",
  rgba: 0
};
var channels;
((channels2) => {
  function toCss(r, g, b, a) {
    if (a !== void 0) {
      return `#${toPaddedHex(r)}${toPaddedHex(g)}${toPaddedHex(b)}${toPaddedHex(a)}`;
    }
    return `#${toPaddedHex(r)}${toPaddedHex(g)}${toPaddedHex(b)}`;
  }
  channels2.toCss = toCss;
  function toRgba(r, g, b, a = 255) {
    return (r << 24 | g << 16 | b << 8 | a) >>> 0;
  }
  channels2.toRgba = toRgba;
  function toColor(r, g, b, a) {
    return {
      css: channels2.toCss(r, g, b, a),
      rgba: channels2.toRgba(r, g, b, a)
    };
  }
  channels2.toColor = toColor;
})(channels || (channels = {}));
var color;
((color2) => {
  function blend(bg, fg) {
    $a = (fg.rgba & 255) / 255;
    if ($a === 1) {
      return {
        css: fg.css,
        rgba: fg.rgba
      };
    }
    const fgR = fg.rgba >> 24 & 255;
    const fgG = fg.rgba >> 16 & 255;
    const fgB = fg.rgba >> 8 & 255;
    const bgR = bg.rgba >> 24 & 255;
    const bgG = bg.rgba >> 16 & 255;
    const bgB = bg.rgba >> 8 & 255;
    $r = bgR + Math.round((fgR - bgR) * $a);
    $g = bgG + Math.round((fgG - bgG) * $a);
    $b = bgB + Math.round((fgB - bgB) * $a);
    const css2 = channels.toCss($r, $g, $b);
    const rgba2 = channels.toRgba($r, $g, $b);
    return { css: css2, rgba: rgba2 };
  }
  color2.blend = blend;
  function isOpaque(color3) {
    return (color3.rgba & 255) === 255;
  }
  color2.isOpaque = isOpaque;
  function ensureContrastRatio(bg, fg, ratio) {
    const result = rgba.ensureContrastRatio(bg.rgba, fg.rgba, ratio);
    if (!result) {
      return void 0;
    }
    return channels.toColor(
      result >> 24 & 255,
      result >> 16 & 255,
      result >> 8 & 255
    );
  }
  color2.ensureContrastRatio = ensureContrastRatio;
  function opaque(color3) {
    const rgbaColor = (color3.rgba | 255) >>> 0;
    [$r, $g, $b] = rgba.toChannels(rgbaColor);
    return {
      css: channels.toCss($r, $g, $b),
      rgba: rgbaColor
    };
  }
  color2.opaque = opaque;
  function opacity(color3, opacity2) {
    $a = Math.round(opacity2 * 255);
    [$r, $g, $b] = rgba.toChannels(color3.rgba);
    return {
      css: channels.toCss($r, $g, $b, $a),
      rgba: channels.toRgba($r, $g, $b, $a)
    };
  }
  color2.opacity = opacity;
  function multiplyOpacity(color3, factor) {
    $a = color3.rgba & 255;
    return opacity(color3, $a * factor / 255);
  }
  color2.multiplyOpacity = multiplyOpacity;
  function toColorRGB(color3) {
    return [color3.rgba >> 24 & 255, color3.rgba >> 16 & 255, color3.rgba >> 8 & 255];
  }
  color2.toColorRGB = toColorRGB;
})(color || (color = {}));
var css;
((css2) => {
  let $ctx;
  let $litmusColor;
  try {
    const canvas = document.createElement("canvas");
    canvas.width = 1;
    canvas.height = 1;
    const ctx = canvas.getContext("2d", {
      willReadFrequently: true
    });
    if (ctx) {
      $ctx = ctx;
      $ctx.globalCompositeOperation = "copy";
      $litmusColor = $ctx.createLinearGradient(0, 0, 1, 1);
    }
  } catch {
  }
  function toColor(css3) {
    if (css3.match(/#[\da-f]{3,8}/i)) {
      switch (css3.length) {
        case 4: {
          $r = parseInt(css3.slice(1, 2).repeat(2), 16);
          $g = parseInt(css3.slice(2, 3).repeat(2), 16);
          $b = parseInt(css3.slice(3, 4).repeat(2), 16);
          return channels.toColor($r, $g, $b);
        }
        case 5: {
          $r = parseInt(css3.slice(1, 2).repeat(2), 16);
          $g = parseInt(css3.slice(2, 3).repeat(2), 16);
          $b = parseInt(css3.slice(3, 4).repeat(2), 16);
          $a = parseInt(css3.slice(4, 5).repeat(2), 16);
          return channels.toColor($r, $g, $b, $a);
        }
        case 7:
          return {
            css: css3,
            rgba: (parseInt(css3.slice(1), 16) << 8 | 255) >>> 0
          };
        case 9:
          return {
            css: css3,
            rgba: parseInt(css3.slice(1), 16) >>> 0
          };
      }
    }
    const rgbaMatch = css3.match(/rgba?\(\s*(\d{1,3})\s*,\s*(\d{1,3})\s*,\s*(\d{1,3})\s*(,\s*(0|1|\d?\.(\d+))\s*)?\)/);
    if (rgbaMatch) {
      $r = parseInt(rgbaMatch[1]);
      $g = parseInt(rgbaMatch[2]);
      $b = parseInt(rgbaMatch[3]);
      $a = Math.round((rgbaMatch[5] === void 0 ? 1 : parseFloat(rgbaMatch[5])) * 255);
      return channels.toColor($r, $g, $b, $a);
    }
    if (!$ctx || !$litmusColor) {
      throw new Error("css.toColor: Unsupported css format");
    }
    $ctx.fillStyle = $litmusColor;
    $ctx.fillStyle = css3;
    if (typeof $ctx.fillStyle !== "string") {
      throw new Error("css.toColor: Unsupported css format");
    }
    $ctx.fillRect(0, 0, 1, 1);
    [$r, $g, $b, $a] = $ctx.getImageData(0, 0, 1, 1).data;
    if ($a !== 255) {
      throw new Error("css.toColor: Unsupported css format");
    }
    return {
      rgba: channels.toRgba($r, $g, $b, $a),
      css: css3
    };
  }
  css2.toColor = toColor;
})(css || (css = {}));
var rgb;
((rgb2) => {
  function relativeLuminance(rgb3) {
    return relativeLuminance2(
      rgb3 >> 16 & 255,
      rgb3 >> 8 & 255,
      rgb3 & 255
    );
  }
  rgb2.relativeLuminance = relativeLuminance;
  function relativeLuminance2(r, g, b) {
    const rs = r / 255;
    const gs = g / 255;
    const bs = b / 255;
    const rr = rs <= 0.03928 ? rs / 12.92 : Math.pow((rs + 0.055) / 1.055, 2.4);
    const rg = gs <= 0.03928 ? gs / 12.92 : Math.pow((gs + 0.055) / 1.055, 2.4);
    const rb = bs <= 0.03928 ? bs / 12.92 : Math.pow((bs + 0.055) / 1.055, 2.4);
    return rr * 0.2126 + rg * 0.7152 + rb * 0.0722;
  }
  rgb2.relativeLuminance2 = relativeLuminance2;
})(rgb || (rgb = {}));
var rgba;
((rgba2) => {
  function blend(bg, fg) {
    $a = (fg & 255) / 255;
    if ($a === 1) {
      return fg;
    }
    const fgR = fg >> 24 & 255;
    const fgG = fg >> 16 & 255;
    const fgB = fg >> 8 & 255;
    const bgR = bg >> 24 & 255;
    const bgG = bg >> 16 & 255;
    const bgB = bg >> 8 & 255;
    $r = bgR + Math.round((fgR - bgR) * $a);
    $g = bgG + Math.round((fgG - bgG) * $a);
    $b = bgB + Math.round((fgB - bgB) * $a);
    return channels.toRgba($r, $g, $b);
  }
  rgba2.blend = blend;
  function ensureContrastRatio(bgRgba, fgRgba, ratio) {
    const bgL = rgb.relativeLuminance(bgRgba >> 8);
    const fgL = rgb.relativeLuminance(fgRgba >> 8);
    const cr = contrastRatio(bgL, fgL);
    if (cr < ratio) {
      if (fgL < bgL) {
        const resultA2 = reduceLuminance(bgRgba, fgRgba, ratio);
        const resultARatio2 = contrastRatio(bgL, rgb.relativeLuminance(resultA2 >> 8));
        if (resultARatio2 < ratio) {
          const resultB = increaseLuminance(bgRgba, fgRgba, ratio);
          const resultBRatio = contrastRatio(bgL, rgb.relativeLuminance(resultB >> 8));
          return resultARatio2 > resultBRatio ? resultA2 : resultB;
        }
        return resultA2;
      }
      const resultA = increaseLuminance(bgRgba, fgRgba, ratio);
      const resultARatio = contrastRatio(bgL, rgb.relativeLuminance(resultA >> 8));
      if (resultARatio < ratio) {
        const resultB = reduceLuminance(bgRgba, fgRgba, ratio);
        const resultBRatio = contrastRatio(bgL, rgb.relativeLuminance(resultB >> 8));
        return resultARatio > resultBRatio ? resultA : resultB;
      }
      return resultA;
    }
    return void 0;
  }
  rgba2.ensureContrastRatio = ensureContrastRatio;
  function reduceLuminance(bgRgba, fgRgba, ratio) {
    const bgR = bgRgba >> 24 & 255;
    const bgG = bgRgba >> 16 & 255;
    const bgB = bgRgba >> 8 & 255;
    let fgR = fgRgba >> 24 & 255;
    let fgG = fgRgba >> 16 & 255;
    let fgB = fgRgba >> 8 & 255;
    let cr = contrastRatio(rgb.relativeLuminance2(fgR, fgG, fgB), rgb.relativeLuminance2(bgR, bgG, bgB));
    while (cr < ratio && (fgR > 0 || fgG > 0 || fgB > 0)) {
      fgR -= Math.max(0, Math.ceil(fgR * 0.1));
      fgG -= Math.max(0, Math.ceil(fgG * 0.1));
      fgB -= Math.max(0, Math.ceil(fgB * 0.1));
      cr = contrastRatio(rgb.relativeLuminance2(fgR, fgG, fgB), rgb.relativeLuminance2(bgR, bgG, bgB));
    }
    return (fgR << 24 | fgG << 16 | fgB << 8 | 255) >>> 0;
  }
  rgba2.reduceLuminance = reduceLuminance;
  function increaseLuminance(bgRgba, fgRgba, ratio) {
    const bgR = bgRgba >> 24 & 255;
    const bgG = bgRgba >> 16 & 255;
    const bgB = bgRgba >> 8 & 255;
    let fgR = fgRgba >> 24 & 255;
    let fgG = fgRgba >> 16 & 255;
    let fgB = fgRgba >> 8 & 255;
    let cr = contrastRatio(rgb.relativeLuminance2(fgR, fgG, fgB), rgb.relativeLuminance2(bgR, bgG, bgB));
    while (cr < ratio && (fgR < 255 || fgG < 255 || fgB < 255)) {
      fgR = Math.min(255, fgR + Math.ceil((255 - fgR) * 0.1));
      fgG = Math.min(255, fgG + Math.ceil((255 - fgG) * 0.1));
      fgB = Math.min(255, fgB + Math.ceil((255 - fgB) * 0.1));
      cr = contrastRatio(rgb.relativeLuminance2(fgR, fgG, fgB), rgb.relativeLuminance2(bgR, bgG, bgB));
    }
    return (fgR << 24 | fgG << 16 | fgB << 8 | 255) >>> 0;
  }
  rgba2.increaseLuminance = increaseLuminance;
  function toChannels(value) {
    return [value >> 24 & 255, value >> 16 & 255, value >> 8 & 255, value & 255];
  }
  rgba2.toChannels = toChannels;
})(rgba || (rgba = {}));
function toPaddedHex(c) {
  const s = c.toString(16);
  return s.length < 2 ? "0" + s : s;
}
function contrastRatio(l1, l2) {
  if (l1 < l2) {
    return (l2 + 0.05) / (l1 + 0.05);
  }
  return (l1 + 0.05) / (l2 + 0.05);
}

// node_modules/@xterm/xterm/src/browser/renderer/shared/RendererUtils.ts
function throwIfFalsy(value) {
  if (!value) {
    throw new Error("value must not be falsy");
  }
  return value;
}
function isPowerlineGlyph(codepoint) {
  return 57508 <= codepoint && codepoint <= 57558;
}
function isRestrictedPowerlineGlyph(codepoint) {
  return 57520 <= codepoint && codepoint <= 57527;
}
function isNerdFontGlyph(codepoint) {
  return 57344 <= codepoint && codepoint <= 63743;
}
function isBoxOrBlockGlyph(codepoint) {
  return 9472 <= codepoint && codepoint <= 9631;
}
function isEmoji(codepoint) {
  return codepoint >= 128512 && codepoint <= 128591 || // Emoticons
  codepoint >= 127744 && codepoint <= 128511 || // Misc Symbols and Pictographs
  codepoint >= 128640 && codepoint <= 128767 || // Transport and Map
  codepoint >= 9728 && codepoint <= 9983 || // Misc symbols
  codepoint >= 9984 && codepoint <= 10175 || // Dingbats
  codepoint >= 65024 && codepoint <= 65039 || // Variation Selectors
  codepoint >= 129280 && codepoint <= 129535 || // Supplemental Symbols and Pictographs
  codepoint >= 127462 && codepoint <= 127487;
}
function allowRescaling(codepoint, width, glyphSizeX, deviceCellWidth) {
  return (
    // Is single cell width
    width === 1 && // Glyph exceeds cell bounds, add 50% to avoid hurting readability by rescaling glyphs that
    // barely overlap
    glyphSizeX > Math.ceil(deviceCellWidth * 1.5) && // Never rescale ascii
    codepoint !== void 0 && codepoint > 255 && // Never rescale emoji
    !isEmoji(codepoint) && // Never rescale powerline or nerd fonts
    !isPowerlineGlyph(codepoint) && !isNerdFontGlyph(codepoint)
  );
}
function treatGlyphAsBackgroundColor(codepoint) {
  return isPowerlineGlyph(codepoint) || isBoxOrBlockGlyph(codepoint);
}
function createRenderDimensions() {
  return {
    css: {
      canvas: createDimension(),
      cell: createDimension()
    },
    device: {
      canvas: createDimension(),
      cell: createDimension(),
      char: {
        width: 0,
        height: 0,
        left: 0,
        top: 0
      }
    }
  };
}
function createDimension() {
  return {
    width: 0,
    height: 0
  };
}
function computeNextVariantOffset(cellWidth, lineWidth, currentOffset = 0) {
  return (cellWidth - (Math.round(lineWidth) * 2 - currentOffset)) % (Math.round(lineWidth) * 2);
}

// vendor/xterm-addon-webgl/src/CellColorResolver.ts
var $fg = 0;
var $bg = 0;
var $hasFg = false;
var $hasBg = false;
var $isSelected = false;
var $colors;
var $variantOffset = 0;
var CellColorResolver = class {
  constructor(_terminal, _optionService, _selectionRenderModel, _decorationService, _coreBrowserService, _themeService) {
    this._terminal = _terminal;
    this._optionService = _optionService;
    this._selectionRenderModel = _selectionRenderModel;
    this._decorationService = _decorationService;
    this._coreBrowserService = _coreBrowserService;
    this._themeService = _themeService;
    /**
     * The shared result of the {@link resolve} call. This is only safe to use immediately after as
     * any other calls will share object.
     */
    this.result = {
      fg: 0,
      bg: 0,
      ext: 0
    };
  }
  /**
   * Resolves colors for the cell, putting the result into the shared {@link result}. This resolves
   * overrides, inverse and selection for the cell which can then be used to feed into the renderer.
   */
  resolve(cell, x, y, deviceCellWidth) {
    this.result.bg = cell.bg;
    this.result.fg = cell.fg;
    this.result.ext = cell.bg & 268435456 /* HAS_EXTENDED */ ? cell.extended.ext : 0;
    $bg = 0;
    $fg = 0;
    $hasBg = false;
    $hasFg = false;
    $isSelected = false;
    $colors = this._themeService.colors;
    $variantOffset = 0;
    const code = cell.getCode();
    if (code !== NULL_CELL_CODE && cell.extended.underlineStyle === 4 /* DOTTED */) {
      const lineWidth = Math.max(1, Math.floor(this._optionService.rawOptions.fontSize * this._coreBrowserService.dpr / 15));
      $variantOffset = x * deviceCellWidth % (Math.round(lineWidth) * 2);
    }
    this._decorationService.forEachDecorationAtCell(x, y, "bottom", (d) => {
      if (d.backgroundColorRGB) {
        $bg = d.backgroundColorRGB.rgba >> 8 & 16777215 /* RGB_MASK */;
        $hasBg = true;
      }
      if (d.foregroundColorRGB) {
        $fg = d.foregroundColorRGB.rgba >> 8 & 16777215 /* RGB_MASK */;
        $hasFg = true;
      }
    });
    $isSelected = this._selectionRenderModel.isCellSelected(this._terminal, x, y);
    if ($isSelected) {
      if (this.result.fg & 67108864 /* INVERSE */ || (this.result.bg & 50331648 /* CM_MASK */) !== 0 /* CM_DEFAULT */) {
        if (this.result.fg & 67108864 /* INVERSE */) {
          switch (this.result.fg & 50331648 /* CM_MASK */) {
            case 16777216 /* CM_P16 */:
            case 33554432 /* CM_P256 */:
              $bg = this._themeService.colors.ansi[this.result.fg & 255 /* PCOLOR_MASK */].rgba;
              break;
            case 50331648 /* CM_RGB */:
              $bg = (this.result.fg & 16777215 /* RGB_MASK */) << 8 | 255;
              break;
            case 0 /* CM_DEFAULT */:
            default:
              $bg = this._themeService.colors.foreground.rgba;
          }
        } else {
          switch (this.result.bg & 50331648 /* CM_MASK */) {
            case 16777216 /* CM_P16 */:
            case 33554432 /* CM_P256 */:
              $bg = this._themeService.colors.ansi[this.result.bg & 255 /* PCOLOR_MASK */].rgba;
              break;
            case 50331648 /* CM_RGB */:
              $bg = (this.result.bg & 16777215 /* RGB_MASK */) << 8 | 255;
              break;
          }
        }
        $bg = rgba.blend(
          $bg,
          (this._coreBrowserService.isFocused ? $colors.selectionBackgroundOpaque : $colors.selectionInactiveBackgroundOpaque).rgba & 4294967040 | 128
        ) >> 8 & 16777215 /* RGB_MASK */;
      } else {
        $bg = (this._coreBrowserService.isFocused ? $colors.selectionBackgroundOpaque : $colors.selectionInactiveBackgroundOpaque).rgba >> 8 & 16777215 /* RGB_MASK */;
      }
      $hasBg = true;
      if ($colors.selectionForeground) {
        $fg = $colors.selectionForeground.rgba >> 8 & 16777215 /* RGB_MASK */;
        $hasFg = true;
      }
      if (treatGlyphAsBackgroundColor(cell.getCode())) {
        if (this.result.fg & 67108864 /* INVERSE */ && (this.result.bg & 50331648 /* CM_MASK */) === 0 /* CM_DEFAULT */) {
          $fg = (this._coreBrowserService.isFocused ? $colors.selectionBackgroundOpaque : $colors.selectionInactiveBackgroundOpaque).rgba >> 8 & 16777215 /* RGB_MASK */;
        } else {
          if (this.result.fg & 67108864 /* INVERSE */) {
            switch (this.result.bg & 50331648 /* CM_MASK */) {
              case 16777216 /* CM_P16 */:
              case 33554432 /* CM_P256 */:
                $fg = this._themeService.colors.ansi[this.result.bg & 255 /* PCOLOR_MASK */].rgba;
                break;
              case 50331648 /* CM_RGB */:
                $fg = (this.result.bg & 16777215 /* RGB_MASK */) << 8 | 255;
                break;
            }
          } else {
            switch (this.result.fg & 50331648 /* CM_MASK */) {
              case 16777216 /* CM_P16 */:
              case 33554432 /* CM_P256 */:
                $fg = this._themeService.colors.ansi[this.result.fg & 255 /* PCOLOR_MASK */].rgba;
                break;
              case 50331648 /* CM_RGB */:
                $fg = (this.result.fg & 16777215 /* RGB_MASK */) << 8 | 255;
                break;
              case 0 /* CM_DEFAULT */:
              default:
                $fg = this._themeService.colors.foreground.rgba;
            }
          }
          $fg = rgba.blend(
            $fg,
            (this._coreBrowserService.isFocused ? $colors.selectionBackgroundOpaque : $colors.selectionInactiveBackgroundOpaque).rgba & 4294967040 | 128
          ) >> 8 & 16777215 /* RGB_MASK */;
        }
        $hasFg = true;
      }
    }
    this._decorationService.forEachDecorationAtCell(x, y, "top", (d) => {
      if (d.backgroundColorRGB) {
        $bg = d.backgroundColorRGB.rgba >> 8 & 16777215 /* RGB_MASK */;
        $hasBg = true;
      }
      if (d.foregroundColorRGB) {
        $fg = d.foregroundColorRGB.rgba >> 8 & 16777215 /* RGB_MASK */;
        $hasFg = true;
      }
    });
    if ($hasBg) {
      if ($isSelected) {
        $bg = cell.bg & ~16777215 /* RGB_MASK */ & ~134217728 /* DIM */ | $bg | 50331648 /* CM_RGB */;
      } else {
        $bg = cell.bg & ~16777215 /* RGB_MASK */ | $bg | 50331648 /* CM_RGB */;
      }
    }
    if ($hasFg) {
      $fg = cell.fg & ~16777215 /* RGB_MASK */ & ~67108864 /* INVERSE */ | $fg | 50331648 /* CM_RGB */;
    }
    if (this.result.fg & 67108864 /* INVERSE */) {
      if ($hasBg && !$hasFg) {
        if ((this.result.bg & 50331648 /* CM_MASK */) === 0 /* CM_DEFAULT */) {
          $fg = this.result.fg & ~(16777215 /* RGB_MASK */ | 67108864 /* INVERSE */ | 50331648 /* CM_MASK */) | $colors.background.rgba >> 8 & 16777215 /* RGB_MASK */ & 16777215 /* RGB_MASK */ | 50331648 /* CM_RGB */;
        } else {
          $fg = this.result.fg & ~(16777215 /* RGB_MASK */ | 67108864 /* INVERSE */ | 50331648 /* CM_MASK */) | this.result.bg & (16777215 /* RGB_MASK */ | 50331648 /* CM_MASK */);
        }
        $hasFg = true;
      }
      if (!$hasBg && $hasFg) {
        if ((this.result.fg & 50331648 /* CM_MASK */) === 0 /* CM_DEFAULT */) {
          $bg = this.result.bg & ~(16777215 /* RGB_MASK */ | 50331648 /* CM_MASK */) | $colors.foreground.rgba >> 8 & 16777215 /* RGB_MASK */ & 16777215 /* RGB_MASK */ | 50331648 /* CM_RGB */;
        } else {
          $bg = this.result.bg & ~(16777215 /* RGB_MASK */ | 50331648 /* CM_MASK */) | this.result.fg & (16777215 /* RGB_MASK */ | 50331648 /* CM_MASK */);
        }
        $hasBg = true;
      }
    }
    $colors = void 0;
    this.result.bg = $hasBg ? $bg : this.result.bg;
    this.result.fg = $hasFg ? $fg : this.result.fg;
    this.result.ext &= ~3758096384 /* VARIANT_OFFSET */;
    this.result.ext |= $variantOffset << 29 & 3758096384 /* VARIANT_OFFSET */;
  }
};

// vendor/xterm-addon-webgl/src/Constants.ts
var DIM_OPACITY = 0.5;
var TEXT_BASELINE = isFirefox || isLegacyEdge ? "bottom" : "ideographic";

// vendor/xterm-addon-webgl/src/CustomGlyphs.ts
var blockElementDefinitions = {
  // Block elements (0x2580-0x2590)
  "\u2580": [{ x: 0, y: 0, w: 8, h: 4 }],
  // UPPER HALF BLOCK
  "\u2581": [{ x: 0, y: 7, w: 8, h: 1 }],
  // LOWER ONE EIGHTH BLOCK
  "\u2582": [{ x: 0, y: 6, w: 8, h: 2 }],
  // LOWER ONE QUARTER BLOCK
  "\u2583": [{ x: 0, y: 5, w: 8, h: 3 }],
  // LOWER THREE EIGHTHS BLOCK
  "\u2584": [{ x: 0, y: 4, w: 8, h: 4 }],
  // LOWER HALF BLOCK
  "\u2585": [{ x: 0, y: 3, w: 8, h: 5 }],
  // LOWER FIVE EIGHTHS BLOCK
  "\u2586": [{ x: 0, y: 2, w: 8, h: 6 }],
  // LOWER THREE QUARTERS BLOCK
  "\u2587": [{ x: 0, y: 1, w: 8, h: 7 }],
  // LOWER SEVEN EIGHTHS BLOCK
  "\u2588": [{ x: 0, y: 0, w: 8, h: 8 }],
  // FULL BLOCK
  "\u2589": [{ x: 0, y: 0, w: 7, h: 8 }],
  // LEFT SEVEN EIGHTHS BLOCK
  "\u258A": [{ x: 0, y: 0, w: 6, h: 8 }],
  // LEFT THREE QUARTERS BLOCK
  "\u258B": [{ x: 0, y: 0, w: 5, h: 8 }],
  // LEFT FIVE EIGHTHS BLOCK
  "\u258C": [{ x: 0, y: 0, w: 4, h: 8 }],
  // LEFT HALF BLOCK
  "\u258D": [{ x: 0, y: 0, w: 3, h: 8 }],
  // LEFT THREE EIGHTHS BLOCK
  "\u258E": [{ x: 0, y: 0, w: 2, h: 8 }],
  // LEFT ONE QUARTER BLOCK
  "\u258F": [{ x: 0, y: 0, w: 1, h: 8 }],
  // LEFT ONE EIGHTH BLOCK
  "\u2590": [{ x: 4, y: 0, w: 4, h: 8 }],
  // RIGHT HALF BLOCK
  // Block elements (0x2594-0x2595)
  "\u2594": [{ x: 0, y: 0, w: 8, h: 1 }],
  // UPPER ONE EIGHTH BLOCK
  "\u2595": [{ x: 7, y: 0, w: 1, h: 8 }],
  // RIGHT ONE EIGHTH BLOCK
  // Terminal graphic characters (0x2596-0x259F)
  "\u2596": [{ x: 0, y: 4, w: 4, h: 4 }],
  // QUADRANT LOWER LEFT
  "\u2597": [{ x: 4, y: 4, w: 4, h: 4 }],
  // QUADRANT LOWER RIGHT
  "\u2598": [{ x: 0, y: 0, w: 4, h: 4 }],
  // QUADRANT UPPER LEFT
  "\u2599": [{ x: 0, y: 0, w: 4, h: 8 }, { x: 0, y: 4, w: 8, h: 4 }],
  // QUADRANT UPPER LEFT AND LOWER LEFT AND LOWER RIGHT
  "\u259A": [{ x: 0, y: 0, w: 4, h: 4 }, { x: 4, y: 4, w: 4, h: 4 }],
  // QUADRANT UPPER LEFT AND LOWER RIGHT
  "\u259B": [{ x: 0, y: 0, w: 4, h: 8 }, { x: 4, y: 0, w: 4, h: 4 }],
  // QUADRANT UPPER LEFT AND UPPER RIGHT AND LOWER LEFT
  "\u259C": [{ x: 0, y: 0, w: 8, h: 4 }, { x: 4, y: 0, w: 4, h: 8 }],
  // QUADRANT UPPER LEFT AND UPPER RIGHT AND LOWER RIGHT
  "\u259D": [{ x: 4, y: 0, w: 4, h: 4 }],
  // QUADRANT UPPER RIGHT
  "\u259E": [{ x: 4, y: 0, w: 4, h: 4 }, { x: 0, y: 4, w: 4, h: 4 }],
  // QUADRANT UPPER RIGHT AND LOWER LEFT
  "\u259F": [{ x: 4, y: 0, w: 4, h: 8 }, { x: 0, y: 4, w: 8, h: 4 }],
  // QUADRANT UPPER RIGHT AND LOWER LEFT AND LOWER RIGHT
  // VERTICAL ONE EIGHTH BLOCK-2 through VERTICAL ONE EIGHTH BLOCK-7
  "\u{1FB70}": [{ x: 1, y: 0, w: 1, h: 8 }],
  "\u{1FB71}": [{ x: 2, y: 0, w: 1, h: 8 }],
  "\u{1FB72}": [{ x: 3, y: 0, w: 1, h: 8 }],
  "\u{1FB73}": [{ x: 4, y: 0, w: 1, h: 8 }],
  "\u{1FB74}": [{ x: 5, y: 0, w: 1, h: 8 }],
  "\u{1FB75}": [{ x: 6, y: 0, w: 1, h: 8 }],
  // HORIZONTAL ONE EIGHTH BLOCK-2 through HORIZONTAL ONE EIGHTH BLOCK-7
  "\u{1FB76}": [{ x: 0, y: 1, w: 8, h: 1 }],
  "\u{1FB77}": [{ x: 0, y: 2, w: 8, h: 1 }],
  "\u{1FB78}": [{ x: 0, y: 3, w: 8, h: 1 }],
  "\u{1FB79}": [{ x: 0, y: 4, w: 8, h: 1 }],
  "\u{1FB7A}": [{ x: 0, y: 5, w: 8, h: 1 }],
  "\u{1FB7B}": [{ x: 0, y: 6, w: 8, h: 1 }],
  // LEFT AND LOWER ONE EIGHTH BLOCK
  "\u{1FB7C}": [{ x: 0, y: 0, w: 1, h: 8 }, { x: 0, y: 7, w: 8, h: 1 }],
  // LEFT AND UPPER ONE EIGHTH BLOCK
  "\u{1FB7D}": [{ x: 0, y: 0, w: 1, h: 8 }, { x: 0, y: 0, w: 8, h: 1 }],
  // RIGHT AND UPPER ONE EIGHTH BLOCK
  "\u{1FB7E}": [{ x: 7, y: 0, w: 1, h: 8 }, { x: 0, y: 0, w: 8, h: 1 }],
  // RIGHT AND LOWER ONE EIGHTH BLOCK
  "\u{1FB7F}": [{ x: 7, y: 0, w: 1, h: 8 }, { x: 0, y: 7, w: 8, h: 1 }],
  // UPPER AND LOWER ONE EIGHTH BLOCK
  "\u{1FB80}": [{ x: 0, y: 0, w: 8, h: 1 }, { x: 0, y: 7, w: 8, h: 1 }],
  // HORIZONTAL ONE EIGHTH BLOCK-1358
  "\u{1FB81}": [{ x: 0, y: 0, w: 8, h: 1 }, { x: 0, y: 2, w: 8, h: 1 }, { x: 0, y: 4, w: 8, h: 1 }, { x: 0, y: 7, w: 8, h: 1 }],
  // UPPER ONE QUARTER BLOCK
  "\u{1FB82}": [{ x: 0, y: 0, w: 8, h: 2 }],
  // UPPER THREE EIGHTHS BLOCK
  "\u{1FB83}": [{ x: 0, y: 0, w: 8, h: 3 }],
  // UPPER FIVE EIGHTHS BLOCK
  "\u{1FB84}": [{ x: 0, y: 0, w: 8, h: 5 }],
  // UPPER THREE QUARTERS BLOCK
  "\u{1FB85}": [{ x: 0, y: 0, w: 8, h: 6 }],
  // UPPER SEVEN EIGHTHS BLOCK
  "\u{1FB86}": [{ x: 0, y: 0, w: 8, h: 7 }],
  // RIGHT ONE QUARTER BLOCK
  "\u{1FB87}": [{ x: 6, y: 0, w: 2, h: 8 }],
  // RIGHT THREE EIGHTHS B0OCK
  "\u{1FB88}": [{ x: 5, y: 0, w: 3, h: 8 }],
  // RIGHT FIVE EIGHTHS BL0CK
  "\u{1FB89}": [{ x: 3, y: 0, w: 5, h: 8 }],
  // RIGHT THREE QUARTERS 0LOCK
  "\u{1FB8A}": [{ x: 2, y: 0, w: 6, h: 8 }],
  // RIGHT SEVEN EIGHTHS B0OCK
  "\u{1FB8B}": [{ x: 1, y: 0, w: 7, h: 8 }],
  // CHECKER BOARD FILL
  "\u{1FB95}": [
    { x: 0, y: 0, w: 2, h: 2 },
    { x: 4, y: 0, w: 2, h: 2 },
    { x: 2, y: 2, w: 2, h: 2 },
    { x: 6, y: 2, w: 2, h: 2 },
    { x: 0, y: 4, w: 2, h: 2 },
    { x: 4, y: 4, w: 2, h: 2 },
    { x: 2, y: 6, w: 2, h: 2 },
    { x: 6, y: 6, w: 2, h: 2 }
  ],
  // INVERSE CHECKER BOARD FILL
  "\u{1FB96}": [
    { x: 2, y: 0, w: 2, h: 2 },
    { x: 6, y: 0, w: 2, h: 2 },
    { x: 0, y: 2, w: 2, h: 2 },
    { x: 4, y: 2, w: 2, h: 2 },
    { x: 2, y: 4, w: 2, h: 2 },
    { x: 6, y: 4, w: 2, h: 2 },
    { x: 0, y: 6, w: 2, h: 2 },
    { x: 4, y: 6, w: 2, h: 2 }
  ],
  // HEAVY HORIZONTAL FILL (upper middle and lower one quarter block)
  "\u{1FB97}": [{ x: 0, y: 2, w: 8, h: 2 }, { x: 0, y: 6, w: 8, h: 2 }]
};
var patternCharacterDefinitions = {
  // Shade characters (0x2591-0x2593)
  "\u2591": [
    // LIGHT SHADE (25%)
    [1, 0, 0, 0],
    [0, 0, 0, 0],
    [0, 0, 1, 0],
    [0, 0, 0, 0]
  ],
  "\u2592": [
    // MEDIUM SHADE (50%)
    [1, 0],
    [0, 0],
    [0, 1],
    [0, 0]
  ],
  "\u2593": [
    // DARK SHADE (75%)
    [0, 1],
    [1, 1],
    [1, 0],
    [1, 1]
  ]
};
var boxDrawingDefinitions = {
  // Uniform normal and bold
  "\u2500": { [1 /* NORMAL */]: "M0,.5 L1,.5" /* LEFT_TO_RIGHT */ },
  "\u2501": { [3 /* BOLD */]: "M0,.5 L1,.5" /* LEFT_TO_RIGHT */ },
  "\u2502": { [1 /* NORMAL */]: "M.5,0 L.5,1" /* TOP_TO_BOTTOM */ },
  "\u2503": { [3 /* BOLD */]: "M.5,0 L.5,1" /* TOP_TO_BOTTOM */ },
  "\u250C": { [1 /* NORMAL */]: "M0.5,1 L.5,.5 L1,.5" /* RIGHT_TO_BOTTOM */ },
  "\u250F": { [3 /* BOLD */]: "M0.5,1 L.5,.5 L1,.5" /* RIGHT_TO_BOTTOM */ },
  "\u2510": { [1 /* NORMAL */]: "M0,.5 L.5,.5 L.5,1" /* LEFT_TO_BOTTOM */ },
  "\u2513": { [3 /* BOLD */]: "M0,.5 L.5,.5 L.5,1" /* LEFT_TO_BOTTOM */ },
  "\u2514": { [1 /* NORMAL */]: "M.5,0 L.5,.5 L1,.5" /* TOP_TO_RIGHT */ },
  "\u2517": { [3 /* BOLD */]: "M.5,0 L.5,.5 L1,.5" /* TOP_TO_RIGHT */ },
  "\u2518": { [1 /* NORMAL */]: "M.5,0 L.5,.5 L0,.5" /* TOP_TO_LEFT */ },
  "\u251B": { [3 /* BOLD */]: "M.5,0 L.5,.5 L0,.5" /* TOP_TO_LEFT */ },
  "\u251C": { [1 /* NORMAL */]: "M.5,0 L.5,1 M.5,.5 L1,.5" /* T_RIGHT */ },
  "\u2523": { [3 /* BOLD */]: "M.5,0 L.5,1 M.5,.5 L1,.5" /* T_RIGHT */ },
  "\u2524": { [1 /* NORMAL */]: "M.5,0 L.5,1 M.5,.5 L0,.5" /* T_LEFT */ },
  "\u252B": { [3 /* BOLD */]: "M.5,0 L.5,1 M.5,.5 L0,.5" /* T_LEFT */ },
  "\u252C": { [1 /* NORMAL */]: "M0,.5 L1,.5 M.5,.5 L.5,1" /* T_BOTTOM */ },
  "\u2533": { [3 /* BOLD */]: "M0,.5 L1,.5 M.5,.5 L.5,1" /* T_BOTTOM */ },
  "\u2534": { [1 /* NORMAL */]: "M0,.5 L1,.5 M.5,.5 L.5,0" /* T_TOP */ },
  "\u253B": { [3 /* BOLD */]: "M0,.5 L1,.5 M.5,.5 L.5,0" /* T_TOP */ },
  "\u253C": { [1 /* NORMAL */]: "M0,.5 L1,.5 M.5,0 L.5,1" /* CROSS */ },
  "\u254B": { [3 /* BOLD */]: "M0,.5 L1,.5 M.5,0 L.5,1" /* CROSS */ },
  "\u2574": { [1 /* NORMAL */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */ },
  "\u2578": { [3 /* BOLD */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */ },
  "\u2575": { [1 /* NORMAL */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */ },
  "\u2579": { [3 /* BOLD */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */ },
  "\u2576": { [1 /* NORMAL */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */ },
  "\u257A": { [3 /* BOLD */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */ },
  "\u2577": { [1 /* NORMAL */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */ },
  "\u257B": { [3 /* BOLD */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */ },
  // Double border
  "\u2550": { [1 /* NORMAL */]: (xp, yp) => `M0,${0.5 - yp} L1,${0.5 - yp} M0,${0.5 + yp} L1,${0.5 + yp}` },
  "\u2551": { [1 /* NORMAL */]: (xp, yp) => `M${0.5 - xp},0 L${0.5 - xp},1 M${0.5 + xp},0 L${0.5 + xp},1` },
  "\u2552": { [1 /* NORMAL */]: (xp, yp) => `M.5,1 L.5,${0.5 - yp} L1,${0.5 - yp} M.5,${0.5 + yp} L1,${0.5 + yp}` },
  "\u2553": { [1 /* NORMAL */]: (xp, yp) => `M${0.5 - xp},1 L${0.5 - xp},.5 L1,.5 M${0.5 + xp},.5 L${0.5 + xp},1` },
  "\u2554": { [1 /* NORMAL */]: (xp, yp) => `M1,${0.5 - yp} L${0.5 - xp},${0.5 - yp} L${0.5 - xp},1 M1,${0.5 + yp} L${0.5 + xp},${0.5 + yp} L${0.5 + xp},1` },
  "\u2555": { [1 /* NORMAL */]: (xp, yp) => `M0,${0.5 - yp} L.5,${0.5 - yp} L.5,1 M0,${0.5 + yp} L.5,${0.5 + yp}` },
  "\u2556": { [1 /* NORMAL */]: (xp, yp) => `M${0.5 + xp},1 L${0.5 + xp},.5 L0,.5 M${0.5 - xp},.5 L${0.5 - xp},1` },
  "\u2557": { [1 /* NORMAL */]: (xp, yp) => `M0,${0.5 + yp} L${0.5 - xp},${0.5 + yp} L${0.5 - xp},1 M0,${0.5 - yp} L${0.5 + xp},${0.5 - yp} L${0.5 + xp},1` },
  "\u2558": { [1 /* NORMAL */]: (xp, yp) => `M.5,0 L.5,${0.5 + yp} L1,${0.5 + yp} M.5,${0.5 - yp} L1,${0.5 - yp}` },
  "\u2559": { [1 /* NORMAL */]: (xp, yp) => `M1,.5 L${0.5 - xp},.5 L${0.5 - xp},0 M${0.5 + xp},.5 L${0.5 + xp},0` },
  "\u255A": { [1 /* NORMAL */]: (xp, yp) => `M1,${0.5 - yp} L${0.5 + xp},${0.5 - yp} L${0.5 + xp},0 M1,${0.5 + yp} L${0.5 - xp},${0.5 + yp} L${0.5 - xp},0` },
  "\u255B": { [1 /* NORMAL */]: (xp, yp) => `M0,${0.5 + yp} L.5,${0.5 + yp} L.5,0 M0,${0.5 - yp} L.5,${0.5 - yp}` },
  "\u255C": { [1 /* NORMAL */]: (xp, yp) => `M0,.5 L${0.5 + xp},.5 L${0.5 + xp},0 M${0.5 - xp},.5 L${0.5 - xp},0` },
  "\u255D": { [1 /* NORMAL */]: (xp, yp) => `M0,${0.5 - yp} L${0.5 - xp},${0.5 - yp} L${0.5 - xp},0 M0,${0.5 + yp} L${0.5 + xp},${0.5 + yp} L${0.5 + xp},0` },
  "\u255E": { [1 /* NORMAL */]: (xp, yp) => `${"M.5,0 L.5,1" /* TOP_TO_BOTTOM */} M.5,${0.5 - yp} L1,${0.5 - yp} M.5,${0.5 + yp} L1,${0.5 + yp}` },
  "\u255F": { [1 /* NORMAL */]: (xp, yp) => `M${0.5 - xp},0 L${0.5 - xp},1 M${0.5 + xp},0 L${0.5 + xp},1 M${0.5 + xp},.5 L1,.5` },
  "\u2560": { [1 /* NORMAL */]: (xp, yp) => `M${0.5 - xp},0 L${0.5 - xp},1 M1,${0.5 + yp} L${0.5 + xp},${0.5 + yp} L${0.5 + xp},1 M1,${0.5 - yp} L${0.5 + xp},${0.5 - yp} L${0.5 + xp},0` },
  "\u2561": { [1 /* NORMAL */]: (xp, yp) => `${"M.5,0 L.5,1" /* TOP_TO_BOTTOM */} M0,${0.5 - yp} L.5,${0.5 - yp} M0,${0.5 + yp} L.5,${0.5 + yp}` },
  "\u2562": { [1 /* NORMAL */]: (xp, yp) => `M0,.5 L${0.5 - xp},.5 M${0.5 - xp},0 L${0.5 - xp},1 M${0.5 + xp},0 L${0.5 + xp},1` },
  "\u2563": { [1 /* NORMAL */]: (xp, yp) => `M${0.5 + xp},0 L${0.5 + xp},1 M0,${0.5 + yp} L${0.5 - xp},${0.5 + yp} L${0.5 - xp},1 M0,${0.5 - yp} L${0.5 - xp},${0.5 - yp} L${0.5 - xp},0` },
  "\u2564": { [1 /* NORMAL */]: (xp, yp) => `M0,${0.5 - yp} L1,${0.5 - yp} M0,${0.5 + yp} L1,${0.5 + yp} M.5,${0.5 + yp} L.5,1` },
  "\u2565": { [1 /* NORMAL */]: (xp, yp) => `${"M0,.5 L1,.5" /* LEFT_TO_RIGHT */} M${0.5 - xp},.5 L${0.5 - xp},1 M${0.5 + xp},.5 L${0.5 + xp},1` },
  "\u2566": { [1 /* NORMAL */]: (xp, yp) => `M0,${0.5 - yp} L1,${0.5 - yp} M0,${0.5 + yp} L${0.5 - xp},${0.5 + yp} L${0.5 - xp},1 M1,${0.5 + yp} L${0.5 + xp},${0.5 + yp} L${0.5 + xp},1` },
  "\u2567": { [1 /* NORMAL */]: (xp, yp) => `M.5,0 L.5,${0.5 - yp} M0,${0.5 - yp} L1,${0.5 - yp} M0,${0.5 + yp} L1,${0.5 + yp}` },
  "\u2568": { [1 /* NORMAL */]: (xp, yp) => `${"M0,.5 L1,.5" /* LEFT_TO_RIGHT */} M${0.5 - xp},.5 L${0.5 - xp},0 M${0.5 + xp},.5 L${0.5 + xp},0` },
  "\u2569": { [1 /* NORMAL */]: (xp, yp) => `M0,${0.5 + yp} L1,${0.5 + yp} M0,${0.5 - yp} L${0.5 - xp},${0.5 - yp} L${0.5 - xp},0 M1,${0.5 - yp} L${0.5 + xp},${0.5 - yp} L${0.5 + xp},0` },
  "\u256A": { [1 /* NORMAL */]: (xp, yp) => `${"M.5,0 L.5,1" /* TOP_TO_BOTTOM */} M0,${0.5 - yp} L1,${0.5 - yp} M0,${0.5 + yp} L1,${0.5 + yp}` },
  "\u256B": { [1 /* NORMAL */]: (xp, yp) => `${"M0,.5 L1,.5" /* LEFT_TO_RIGHT */} M${0.5 - xp},0 L${0.5 - xp},1 M${0.5 + xp},0 L${0.5 + xp},1` },
  "\u256C": { [1 /* NORMAL */]: (xp, yp) => `M0,${0.5 + yp} L${0.5 - xp},${0.5 + yp} L${0.5 - xp},1 M1,${0.5 + yp} L${0.5 + xp},${0.5 + yp} L${0.5 + xp},1 M0,${0.5 - yp} L${0.5 - xp},${0.5 - yp} L${0.5 - xp},0 M1,${0.5 - yp} L${0.5 + xp},${0.5 - yp} L${0.5 + xp},0` },
  // Diagonal
  "\u2571": { [1 /* NORMAL */]: "M1,0 L0,1" },
  "\u2572": { [1 /* NORMAL */]: "M0,0 L1,1" },
  "\u2573": { [1 /* NORMAL */]: "M1,0 L0,1 M0,0 L1,1" },
  // Mixed weight
  "\u257C": { [1 /* NORMAL */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */, [3 /* BOLD */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */ },
  "\u257D": { [1 /* NORMAL */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */, [3 /* BOLD */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */ },
  "\u257E": { [1 /* NORMAL */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */, [3 /* BOLD */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */ },
  "\u257F": { [1 /* NORMAL */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */, [3 /* BOLD */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */ },
  "\u250D": { [1 /* NORMAL */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */, [3 /* BOLD */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */ },
  "\u250E": { [1 /* NORMAL */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */, [3 /* BOLD */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */ },
  "\u2511": { [1 /* NORMAL */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */, [3 /* BOLD */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */ },
  "\u2512": { [1 /* NORMAL */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */, [3 /* BOLD */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */ },
  "\u2515": { [1 /* NORMAL */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */, [3 /* BOLD */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */ },
  "\u2516": { [1 /* NORMAL */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */, [3 /* BOLD */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */ },
  "\u2519": { [1 /* NORMAL */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */, [3 /* BOLD */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */ },
  "\u251A": { [1 /* NORMAL */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */, [3 /* BOLD */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */ },
  "\u251D": { [1 /* NORMAL */]: "M.5,0 L.5,1" /* TOP_TO_BOTTOM */, [3 /* BOLD */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */ },
  "\u251E": { [1 /* NORMAL */]: "M0.5,1 L.5,.5 L1,.5" /* RIGHT_TO_BOTTOM */, [3 /* BOLD */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */ },
  "\u251F": { [1 /* NORMAL */]: "M.5,0 L.5,.5 L1,.5" /* TOP_TO_RIGHT */, [3 /* BOLD */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */ },
  "\u2520": { [1 /* NORMAL */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */, [3 /* BOLD */]: "M.5,0 L.5,1" /* TOP_TO_BOTTOM */ },
  "\u2521": { [1 /* NORMAL */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */, [3 /* BOLD */]: "M.5,0 L.5,.5 L1,.5" /* TOP_TO_RIGHT */ },
  "\u2522": { [1 /* NORMAL */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */, [3 /* BOLD */]: "M0.5,1 L.5,.5 L1,.5" /* RIGHT_TO_BOTTOM */ },
  "\u2525": { [1 /* NORMAL */]: "M.5,0 L.5,1" /* TOP_TO_BOTTOM */, [3 /* BOLD */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */ },
  "\u2526": { [1 /* NORMAL */]: "M0,.5 L.5,.5 L.5,1" /* LEFT_TO_BOTTOM */, [3 /* BOLD */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */ },
  "\u2527": { [1 /* NORMAL */]: "M.5,0 L.5,.5 L0,.5" /* TOP_TO_LEFT */, [3 /* BOLD */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */ },
  "\u2528": { [1 /* NORMAL */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */, [3 /* BOLD */]: "M.5,0 L.5,1" /* TOP_TO_BOTTOM */ },
  "\u2529": { [1 /* NORMAL */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */, [3 /* BOLD */]: "M.5,0 L.5,.5 L0,.5" /* TOP_TO_LEFT */ },
  "\u252A": { [1 /* NORMAL */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */, [3 /* BOLD */]: "M0,.5 L.5,.5 L.5,1" /* LEFT_TO_BOTTOM */ },
  "\u252D": { [1 /* NORMAL */]: "M0.5,1 L.5,.5 L1,.5" /* RIGHT_TO_BOTTOM */, [3 /* BOLD */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */ },
  "\u252E": { [1 /* NORMAL */]: "M0,.5 L.5,.5 L.5,1" /* LEFT_TO_BOTTOM */, [3 /* BOLD */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */ },
  "\u252F": { [1 /* NORMAL */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */, [3 /* BOLD */]: "M0,.5 L1,.5" /* LEFT_TO_RIGHT */ },
  "\u2530": { [1 /* NORMAL */]: "M0,.5 L1,.5" /* LEFT_TO_RIGHT */, [3 /* BOLD */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */ },
  "\u2531": { [1 /* NORMAL */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */, [3 /* BOLD */]: "M0,.5 L.5,.5 L.5,1" /* LEFT_TO_BOTTOM */ },
  "\u2532": { [1 /* NORMAL */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */, [3 /* BOLD */]: "M0.5,1 L.5,.5 L1,.5" /* RIGHT_TO_BOTTOM */ },
  "\u2535": { [1 /* NORMAL */]: "M.5,0 L.5,.5 L1,.5" /* TOP_TO_RIGHT */, [3 /* BOLD */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */ },
  "\u2536": { [1 /* NORMAL */]: "M.5,0 L.5,.5 L0,.5" /* TOP_TO_LEFT */, [3 /* BOLD */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */ },
  "\u2537": { [1 /* NORMAL */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */, [3 /* BOLD */]: "M0,.5 L1,.5" /* LEFT_TO_RIGHT */ },
  "\u2538": { [1 /* NORMAL */]: "M0,.5 L1,.5" /* LEFT_TO_RIGHT */, [3 /* BOLD */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */ },
  "\u2539": { [1 /* NORMAL */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */, [3 /* BOLD */]: "M.5,0 L.5,.5 L0,.5" /* TOP_TO_LEFT */ },
  "\u253A": { [1 /* NORMAL */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */, [3 /* BOLD */]: "M.5,0 L.5,.5 L1,.5" /* TOP_TO_RIGHT */ },
  "\u253D": { [1 /* NORMAL */]: `${"M.5,0 L.5,1" /* TOP_TO_BOTTOM */} ${"M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */}`, [3 /* BOLD */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */ },
  "\u253E": { [1 /* NORMAL */]: `${"M.5,0 L.5,1" /* TOP_TO_BOTTOM */} ${"M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */}`, [3 /* BOLD */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */ },
  "\u253F": { [1 /* NORMAL */]: "M.5,0 L.5,1" /* TOP_TO_BOTTOM */, [3 /* BOLD */]: "M0,.5 L1,.5" /* LEFT_TO_RIGHT */ },
  "\u2540": { [1 /* NORMAL */]: `${"M0,.5 L1,.5" /* LEFT_TO_RIGHT */} ${"M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */}`, [3 /* BOLD */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */ },
  "\u2541": { [1 /* NORMAL */]: `${"M.5,.5 L.5,0" /* MIDDLE_TO_TOP */} ${"M0,.5 L1,.5" /* LEFT_TO_RIGHT */}`, [3 /* BOLD */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */ },
  "\u2542": { [1 /* NORMAL */]: "M0,.5 L1,.5" /* LEFT_TO_RIGHT */, [3 /* BOLD */]: "M.5,0 L.5,1" /* TOP_TO_BOTTOM */ },
  "\u2543": { [1 /* NORMAL */]: "M0.5,1 L.5,.5 L1,.5" /* RIGHT_TO_BOTTOM */, [3 /* BOLD */]: "M.5,0 L.5,.5 L0,.5" /* TOP_TO_LEFT */ },
  "\u2544": { [1 /* NORMAL */]: "M0,.5 L.5,.5 L.5,1" /* LEFT_TO_BOTTOM */, [3 /* BOLD */]: "M.5,0 L.5,.5 L1,.5" /* TOP_TO_RIGHT */ },
  "\u2545": { [1 /* NORMAL */]: "M.5,0 L.5,.5 L1,.5" /* TOP_TO_RIGHT */, [3 /* BOLD */]: "M0,.5 L.5,.5 L.5,1" /* LEFT_TO_BOTTOM */ },
  "\u2546": { [1 /* NORMAL */]: "M.5,0 L.5,.5 L0,.5" /* TOP_TO_LEFT */, [3 /* BOLD */]: "M0.5,1 L.5,.5 L1,.5" /* RIGHT_TO_BOTTOM */ },
  "\u2547": { [1 /* NORMAL */]: "M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */, [3 /* BOLD */]: `${"M.5,.5 L.5,0" /* MIDDLE_TO_TOP */} ${"M0,.5 L1,.5" /* LEFT_TO_RIGHT */}` },
  "\u2548": { [1 /* NORMAL */]: "M.5,.5 L.5,0" /* MIDDLE_TO_TOP */, [3 /* BOLD */]: `${"M0,.5 L1,.5" /* LEFT_TO_RIGHT */} ${"M.5,.5 L.5,1" /* MIDDLE_TO_BOTTOM */}` },
  "\u2549": { [1 /* NORMAL */]: "M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */, [3 /* BOLD */]: `${"M.5,0 L.5,1" /* TOP_TO_BOTTOM */} ${"M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */}` },
  "\u254A": { [1 /* NORMAL */]: "M.5,.5 L0,.5" /* MIDDLE_TO_LEFT */, [3 /* BOLD */]: `${"M.5,0 L.5,1" /* TOP_TO_BOTTOM */} ${"M.5,.5 L1,.5" /* MIDDLE_TO_RIGHT */}` },
  // Dashed
  "\u254C": { [1 /* NORMAL */]: "M.1,.5 L.4,.5 M.6,.5 L.9,.5" /* TWO_DASHES_HORIZONTAL */ },
  "\u254D": { [3 /* BOLD */]: "M.1,.5 L.4,.5 M.6,.5 L.9,.5" /* TWO_DASHES_HORIZONTAL */ },
  "\u2504": { [1 /* NORMAL */]: "M.0667,.5 L.2667,.5 M.4,.5 L.6,.5 M.7333,.5 L.9333,.5" /* THREE_DASHES_HORIZONTAL */ },
  "\u2505": { [3 /* BOLD */]: "M.0667,.5 L.2667,.5 M.4,.5 L.6,.5 M.7333,.5 L.9333,.5" /* THREE_DASHES_HORIZONTAL */ },
  "\u2508": { [1 /* NORMAL */]: "M.05,.5 L.2,.5 M.3,.5 L.45,.5 M.55,.5 L.7,.5 M.8,.5 L.95,.5" /* FOUR_DASHES_HORIZONTAL */ },
  "\u2509": { [3 /* BOLD */]: "M.05,.5 L.2,.5 M.3,.5 L.45,.5 M.55,.5 L.7,.5 M.8,.5 L.95,.5" /* FOUR_DASHES_HORIZONTAL */ },
  "\u254E": { [1 /* NORMAL */]: "M.5,.1 L.5,.4 M.5,.6 L.5,.9" /* TWO_DASHES_VERTICAL */ },
  "\u254F": { [3 /* BOLD */]: "M.5,.1 L.5,.4 M.5,.6 L.5,.9" /* TWO_DASHES_VERTICAL */ },
  "\u2506": { [1 /* NORMAL */]: "M.5,.0667 L.5,.2667 M.5,.4 L.5,.6 M.5,.7333 L.5,.9333" /* THREE_DASHES_VERTICAL */ },
  "\u2507": { [3 /* BOLD */]: "M.5,.0667 L.5,.2667 M.5,.4 L.5,.6 M.5,.7333 L.5,.9333" /* THREE_DASHES_VERTICAL */ },
  "\u250A": { [1 /* NORMAL */]: "M.5,.05 L.5,.2 M.5,.3 L.5,.45 L.5,.55 M.5,.7 L.5,.95" /* FOUR_DASHES_VERTICAL */ },
  "\u250B": { [3 /* BOLD */]: "M.5,.05 L.5,.2 M.5,.3 L.5,.45 L.5,.55 M.5,.7 L.5,.95" /* FOUR_DASHES_VERTICAL */ },
  // Curved
  "\u256D": { [1 /* NORMAL */]: (xp, yp) => `M.5,1 L.5,${0.5 + yp / 0.15 * 0.5} C.5,${0.5 + yp / 0.15 * 0.5},.5,.5,1,.5` },
  "\u256E": { [1 /* NORMAL */]: (xp, yp) => `M.5,1 L.5,${0.5 + yp / 0.15 * 0.5} C.5,${0.5 + yp / 0.15 * 0.5},.5,.5,0,.5` },
  "\u256F": { [1 /* NORMAL */]: (xp, yp) => `M.5,0 L.5,${0.5 - yp / 0.15 * 0.5} C.5,${0.5 - yp / 0.15 * 0.5},.5,.5,0,.5` },
  "\u2570": { [1 /* NORMAL */]: (xp, yp) => `M.5,0 L.5,${0.5 - yp / 0.15 * 0.5} C.5,${0.5 - yp / 0.15 * 0.5},.5,.5,1,.5` }
};
var powerlineDefinitions = {
  // Git branch
  "\uE0A0": { d: "M.3,1 L.03,1 L.03,.88 C.03,.82,.06,.78,.11,.73 C.15,.7,.2,.68,.28,.65 L.43,.6 C.49,.58,.53,.56,.56,.53 C.59,.5,.6,.47,.6,.43 L.6,.27 L.4,.27 L.69,.1 L.98,.27 L.78,.27 L.78,.46 C.78,.52,.76,.56,.72,.61 C.68,.66,.63,.67,.56,.7 L.48,.72 C.42,.74,.38,.76,.35,.78 C.32,.8,.31,.84,.31,.88 L.31,1 M.3,.5 L.03,.59 L.03,.09 L.3,.09 L.3,.655", type: 0 /* FILL */ },
  // L N
  "\uE0A1": { d: "M.7,.4 L.7,.47 L.2,.47 L.2,.03 L.355,.03 L.355,.4 L.705,.4 M.7,.5 L.86,.5 L.86,.95 L.69,.95 L.44,.66 L.46,.86 L.46,.95 L.3,.95 L.3,.49 L.46,.49 L.71,.78 L.69,.565 L.69,.5", type: 0 /* FILL */ },
  // Lock
  "\uE0A2": { d: "M.25,.94 C.16,.94,.11,.92,.11,.87 L.11,.53 C.11,.48,.15,.455,.23,.45 L.23,.3 C.23,.25,.26,.22,.31,.19 C.36,.16,.43,.15,.51,.15 C.59,.15,.66,.16,.71,.19 C.77,.22,.79,.26,.79,.3 L.79,.45 C.87,.45,.91,.48,.91,.53 L.91,.87 C.91,.92,.86,.94,.77,.94 L.24,.94 M.53,.2 C.49,.2,.45,.21,.42,.23 C.39,.25,.38,.27,.38,.3 L.38,.45 L.68,.45 L.68,.3 C.68,.27,.67,.25,.64,.23 C.61,.21,.58,.2,.53,.2 M.58,.82 L.58,.66 C.63,.65,.65,.63,.65,.6 C.65,.58,.64,.57,.61,.56 C.58,.55,.56,.54,.52,.54 C.48,.54,.46,.55,.43,.56 C.4,.57,.39,.59,.39,.6 C.39,.63,.41,.64,.46,.66 L.46,.82 L.57,.82", type: 0 /* FILL */ },
  // Right triangle solid
  "\uE0B0": { d: "M0,0 L1,.5 L0,1", type: 0 /* FILL */, rightPadding: 2 },
  // Right triangle line
  "\uE0B1": { d: "M-1,-.5 L1,.5 L-1,1.5", type: 1 /* STROKE */, leftPadding: 1, rightPadding: 1 },
  // Left triangle solid
  "\uE0B2": { d: "M1,0 L0,.5 L1,1", type: 0 /* FILL */, leftPadding: 2 },
  // Left triangle line
  "\uE0B3": { d: "M2,-.5 L0,.5 L2,1.5", type: 1 /* STROKE */, leftPadding: 1, rightPadding: 1 },
  // Right semi-circle solid
  "\uE0B4": { d: "M0,0 L0,1 C0.552,1,1,0.776,1,.5 C1,0.224,0.552,0,0,0", type: 0 /* FILL */, rightPadding: 1 },
  // Right semi-circle line
  "\uE0B5": { d: "M.2,1 C.422,1,.8,.826,.78,.5 C.8,.174,0.422,0,.2,0", type: 1 /* STROKE */, rightPadding: 1 },
  // Left semi-circle solid
  "\uE0B6": { d: "M1,0 L1,1 C0.448,1,0,0.776,0,.5 C0,0.224,0.448,0,1,0", type: 0 /* FILL */, leftPadding: 1 },
  // Left semi-circle line
  "\uE0B7": { d: "M.8,1 C0.578,1,0.2,.826,.22,.5 C0.2,0.174,0.578,0,0.8,0", type: 1 /* STROKE */, leftPadding: 1 },
  // Lower left triangle
  "\uE0B8": { d: "M-.5,-.5 L1.5,1.5 L-.5,1.5", type: 0 /* FILL */ },
  // Backslash separator
  "\uE0B9": { d: "M-.5,-.5 L1.5,1.5", type: 1 /* STROKE */, leftPadding: 1, rightPadding: 1 },
  // Lower right triangle
  "\uE0BA": { d: "M1.5,-.5 L-.5,1.5 L1.5,1.5", type: 0 /* FILL */ },
  // Upper left triangle
  "\uE0BC": { d: "M1.5,-.5 L-.5,1.5 L-.5,-.5", type: 0 /* FILL */ },
  // Forward slash separator
  "\uE0BD": { d: "M1.5,-.5 L-.5,1.5", type: 1 /* STROKE */, leftPadding: 1, rightPadding: 1 },
  // Upper right triangle
  "\uE0BE": { d: "M-.5,-.5 L1.5,1.5 L1.5,-.5", type: 0 /* FILL */ }
};
powerlineDefinitions["\uE0BB"] = powerlineDefinitions["\uE0BD"];
powerlineDefinitions["\uE0BF"] = powerlineDefinitions["\uE0B9"];
function tryDrawCustomChar(ctx, c, xOffset, yOffset, deviceCellWidth, deviceCellHeight, fontSize, devicePixelRatio) {
  const blockElementDefinition = blockElementDefinitions[c];
  if (blockElementDefinition) {
    drawBlockElementChar(ctx, blockElementDefinition, xOffset, yOffset, deviceCellWidth, deviceCellHeight);
    return true;
  }
  const patternDefinition = patternCharacterDefinitions[c];
  if (patternDefinition) {
    drawPatternChar(ctx, patternDefinition, xOffset, yOffset, deviceCellWidth, deviceCellHeight);
    return true;
  }
  const boxDrawingDefinition = boxDrawingDefinitions[c];
  if (boxDrawingDefinition) {
    drawBoxDrawingChar(ctx, boxDrawingDefinition, xOffset, yOffset, deviceCellWidth, deviceCellHeight, devicePixelRatio);
    return true;
  }
  const powerlineDefinition = powerlineDefinitions[c];
  if (powerlineDefinition) {
    drawPowerlineChar(ctx, powerlineDefinition, xOffset, yOffset, deviceCellWidth, deviceCellHeight, fontSize, devicePixelRatio);
    return true;
  }
  return false;
}
function drawBlockElementChar(ctx, charDefinition, xOffset, yOffset, deviceCellWidth, deviceCellHeight) {
  for (let i = 0; i < charDefinition.length; i++) {
    const box = charDefinition[i];
    const xEighth = deviceCellWidth / 8;
    const yEighth = deviceCellHeight / 8;
    ctx.fillRect(
      xOffset + box.x * xEighth,
      yOffset + box.y * yEighth,
      box.w * xEighth,
      box.h * yEighth
    );
  }
}
var cachedPatterns = /* @__PURE__ */ new Map();
function drawPatternChar(ctx, charDefinition, xOffset, yOffset, deviceCellWidth, deviceCellHeight) {
  let patternSet = cachedPatterns.get(charDefinition);
  if (!patternSet) {
    patternSet = /* @__PURE__ */ new Map();
    cachedPatterns.set(charDefinition, patternSet);
  }
  const fillStyle = ctx.fillStyle;
  if (typeof fillStyle !== "string") {
    throw new Error(`Unexpected fillStyle type "${fillStyle}"`);
  }
  let pattern = patternSet.get(fillStyle);
  if (!pattern) {
    const width = charDefinition[0].length;
    const height = charDefinition.length;
    const tmpCanvas = ctx.canvas.ownerDocument.createElement("canvas");
    tmpCanvas.width = width;
    tmpCanvas.height = height;
    const tmpCtx = throwIfFalsy(tmpCanvas.getContext("2d"));
    const imageData = new ImageData(width, height);
    let r;
    let g;
    let b;
    let a;
    if (fillStyle.startsWith("#")) {
      r = parseInt(fillStyle.slice(1, 3), 16);
      g = parseInt(fillStyle.slice(3, 5), 16);
      b = parseInt(fillStyle.slice(5, 7), 16);
      a = fillStyle.length > 7 && parseInt(fillStyle.slice(7, 9), 16) || 1;
    } else if (fillStyle.startsWith("rgba")) {
      [r, g, b, a] = fillStyle.substring(5, fillStyle.length - 1).split(",").map((e) => parseFloat(e));
    } else {
      throw new Error(`Unexpected fillStyle color format "${fillStyle}" when drawing pattern glyph`);
    }
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        imageData.data[(y * width + x) * 4] = r;
        imageData.data[(y * width + x) * 4 + 1] = g;
        imageData.data[(y * width + x) * 4 + 2] = b;
        imageData.data[(y * width + x) * 4 + 3] = charDefinition[y][x] * (a * 255);
      }
    }
    tmpCtx.putImageData(imageData, 0, 0);
    pattern = throwIfFalsy(ctx.createPattern(tmpCanvas, null));
    patternSet.set(fillStyle, pattern);
  }
  ctx.fillStyle = pattern;
  ctx.fillRect(xOffset, yOffset, deviceCellWidth, deviceCellHeight);
}
function drawBoxDrawingChar(ctx, charDefinition, xOffset, yOffset, deviceCellWidth, deviceCellHeight, devicePixelRatio) {
  ctx.strokeStyle = ctx.fillStyle;
  for (const [fontWeight, instructions] of Object.entries(charDefinition)) {
    ctx.beginPath();
    ctx.lineWidth = devicePixelRatio * Number.parseInt(fontWeight);
    let actualInstructions;
    if (typeof instructions === "function") {
      const xp = 0.15;
      const yp = 0.15 / deviceCellHeight * deviceCellWidth;
      actualInstructions = instructions(xp, yp);
    } else {
      actualInstructions = instructions;
    }
    for (const instruction of actualInstructions.split(" ")) {
      const type = instruction[0];
      const f = svgToCanvasInstructionMap[type];
      if (!f) {
        console.error(`Could not find drawing instructions for "${type}"`);
        continue;
      }
      const args = instruction.substring(1).split(",");
      if (!args[0] || !args[1]) {
        continue;
      }
      f(ctx, translateArgs(args, deviceCellWidth, deviceCellHeight, xOffset, yOffset, true, devicePixelRatio));
    }
    ctx.stroke();
    ctx.closePath();
  }
}
function drawPowerlineChar(ctx, charDefinition, xOffset, yOffset, deviceCellWidth, deviceCellHeight, fontSize, devicePixelRatio) {
  const clipRegion = new Path2D();
  clipRegion.rect(xOffset, yOffset, deviceCellWidth, deviceCellHeight);
  ctx.clip(clipRegion);
  ctx.beginPath();
  const cssLineWidth = fontSize / 12;
  ctx.lineWidth = devicePixelRatio * cssLineWidth;
  for (const instruction of charDefinition.d.split(" ")) {
    const type = instruction[0];
    const f = svgToCanvasInstructionMap[type];
    if (!f) {
      console.error(`Could not find drawing instructions for "${type}"`);
      continue;
    }
    const args = instruction.substring(1).split(",");
    if (!args[0] || !args[1]) {
      continue;
    }
    f(ctx, translateArgs(
      args,
      deviceCellWidth,
      deviceCellHeight,
      xOffset,
      yOffset,
      false,
      devicePixelRatio,
      (charDefinition.leftPadding ?? 0) * (cssLineWidth / 2),
      (charDefinition.rightPadding ?? 0) * (cssLineWidth / 2)
    ));
  }
  if (charDefinition.type === 1 /* STROKE */) {
    ctx.strokeStyle = ctx.fillStyle;
    ctx.stroke();
  } else {
    ctx.fill();
  }
  ctx.closePath();
}
function clamp(value, max, min = 0) {
  return Math.max(Math.min(value, max), min);
}
var svgToCanvasInstructionMap = {
  "C": (ctx, args) => ctx.bezierCurveTo(args[0], args[1], args[2], args[3], args[4], args[5]),
  "L": (ctx, args) => ctx.lineTo(args[0], args[1]),
  "M": (ctx, args) => ctx.moveTo(args[0], args[1])
};
function translateArgs(args, cellWidth, cellHeight, xOffset, yOffset, doClamp, devicePixelRatio, leftPadding = 0, rightPadding = 0) {
  const result = args.map((e) => parseFloat(e) || parseInt(e));
  if (result.length < 2) {
    throw new Error("Too few arguments for instruction");
  }
  for (let x = 0; x < result.length; x += 2) {
    result[x] *= cellWidth - leftPadding * devicePixelRatio - rightPadding * devicePixelRatio;
    if (doClamp && result[x] !== 0) {
      result[x] = clamp(Math.round(result[x] + 0.5) - 0.5, cellWidth, 0);
    }
    result[x] += xOffset + leftPadding * devicePixelRatio;
  }
  for (let y = 1; y < result.length; y += 2) {
    result[y] *= cellHeight;
    if (doClamp && result[y] !== 0) {
      result[y] = clamp(Math.round(result[y] + 0.5) - 0.5, cellHeight, 0);
    }
    result[y] += yOffset;
  }
  return result;
}

// node_modules/@xterm/xterm/src/common/MultiKeyMap.ts
var TwoKeyMap = class {
  constructor() {
    this._data = {};
  }
  set(first, second, value) {
    if (!this._data[first]) {
      this._data[first] = {};
    }
    this._data[first][second] = value;
  }
  get(first, second) {
    return this._data[first] ? this._data[first][second] : void 0;
  }
  clear() {
    this._data = {};
  }
};
var FourKeyMap = class {
  constructor() {
    this._data = new TwoKeyMap();
  }
  set(first, second, third, fourth, value) {
    if (!this._data.get(first, second)) {
      this._data.set(first, second, new TwoKeyMap());
    }
    this._data.get(first, second).set(third, fourth, value);
  }
  get(first, second, third, fourth) {
    return this._data.get(first, second)?.get(third, fourth);
  }
  clear() {
    this._data.clear();
  }
};

// node_modules/@xterm/xterm/src/common/TaskQueue.ts
var TaskQueue = class {
  constructor() {
    this._tasks = [];
    this._i = 0;
  }
  enqueue(task) {
    this._tasks.push(task);
    this._start();
  }
  flush() {
    while (this._i < this._tasks.length) {
      if (!this._tasks[this._i]()) {
        this._i++;
      }
    }
    this.clear();
  }
  clear() {
    if (this._idleCallback) {
      this._cancelCallback(this._idleCallback);
      this._idleCallback = void 0;
    }
    this._i = 0;
    this._tasks.length = 0;
  }
  _start() {
    if (!this._idleCallback) {
      this._idleCallback = this._requestCallback(this._process.bind(this));
    }
  }
  _process(deadline) {
    this._idleCallback = void 0;
    let taskDuration = 0;
    let longestTask = 0;
    let lastDeadlineRemaining = deadline.timeRemaining();
    let deadlineRemaining = 0;
    while (this._i < this._tasks.length) {
      taskDuration = performance.now();
      if (!this._tasks[this._i]()) {
        this._i++;
      }
      taskDuration = Math.max(1, performance.now() - taskDuration);
      longestTask = Math.max(taskDuration, longestTask);
      deadlineRemaining = deadline.timeRemaining();
      if (longestTask * 1.5 > deadlineRemaining) {
        if (lastDeadlineRemaining - taskDuration < -20) {
          console.warn(`task queue exceeded allotted deadline by ${Math.abs(Math.round(lastDeadlineRemaining - taskDuration))}ms`);
        }
        this._start();
        return;
      }
      lastDeadlineRemaining = deadlineRemaining;
    }
    this.clear();
  }
};
var PriorityTaskQueue = class extends TaskQueue {
  _requestCallback(callback) {
    return setTimeout(() => callback(this._createDeadline(16)));
  }
  _cancelCallback(identifier) {
    clearTimeout(identifier);
  }
  _createDeadline(duration) {
    const end = performance.now() + duration;
    return {
      timeRemaining: () => Math.max(0, end - performance.now())
    };
  }
};
var IdleTaskQueueInternal = class extends TaskQueue {
  _requestCallback(callback) {
    return requestIdleCallback(callback);
  }
  _cancelCallback(identifier) {
    cancelIdleCallback(identifier);
  }
};
var IdleTaskQueue = !isNode && "requestIdleCallback" in window ? IdleTaskQueueInternal : PriorityTaskQueue;

// node_modules/@xterm/xterm/src/common/buffer/AttributeData.ts
var AttributeData = class _AttributeData {
  constructor() {
    // data
    this.fg = 0;
    this.bg = 0;
    this.extended = new ExtendedAttrs();
  }
  static toColorRGB(value) {
    return [
      value >>> 16 /* RED_SHIFT */ & 255,
      value >>> 8 /* GREEN_SHIFT */ & 255,
      value & 255
    ];
  }
  static fromColorRGB(value) {
    return (value[0] & 255) << 16 /* RED_SHIFT */ | (value[1] & 255) << 8 /* GREEN_SHIFT */ | value[2] & 255;
  }
  clone() {
    const newObj = new _AttributeData();
    newObj.fg = this.fg;
    newObj.bg = this.bg;
    newObj.extended = this.extended.clone();
    return newObj;
  }
  // flags
  isInverse() {
    return this.fg & 67108864 /* INVERSE */;
  }
  isBold() {
    return this.fg & 134217728 /* BOLD */;
  }
  isUnderline() {
    if (this.hasExtendedAttrs() && this.extended.underlineStyle !== 0 /* NONE */) {
      return 1;
    }
    return this.fg & 268435456 /* UNDERLINE */;
  }
  isBlink() {
    return this.fg & 536870912 /* BLINK */;
  }
  isInvisible() {
    return this.fg & 1073741824 /* INVISIBLE */;
  }
  isItalic() {
    return this.bg & 67108864 /* ITALIC */;
  }
  isDim() {
    return this.bg & 134217728 /* DIM */;
  }
  isStrikethrough() {
    return this.fg & 2147483648 /* STRIKETHROUGH */;
  }
  isProtected() {
    return this.bg & 536870912 /* PROTECTED */;
  }
  isOverline() {
    return this.bg & 1073741824 /* OVERLINE */;
  }
  // color modes
  getFgColorMode() {
    return this.fg & 50331648 /* CM_MASK */;
  }
  getBgColorMode() {
    return this.bg & 50331648 /* CM_MASK */;
  }
  isFgRGB() {
    return (this.fg & 50331648 /* CM_MASK */) === 50331648 /* CM_RGB */;
  }
  isBgRGB() {
    return (this.bg & 50331648 /* CM_MASK */) === 50331648 /* CM_RGB */;
  }
  isFgPalette() {
    return (this.fg & 50331648 /* CM_MASK */) === 16777216 /* CM_P16 */ || (this.fg & 50331648 /* CM_MASK */) === 33554432 /* CM_P256 */;
  }
  isBgPalette() {
    return (this.bg & 50331648 /* CM_MASK */) === 16777216 /* CM_P16 */ || (this.bg & 50331648 /* CM_MASK */) === 33554432 /* CM_P256 */;
  }
  isFgDefault() {
    return (this.fg & 50331648 /* CM_MASK */) === 0;
  }
  isBgDefault() {
    return (this.bg & 50331648 /* CM_MASK */) === 0;
  }
  isAttributeDefault() {
    return this.fg === 0 && this.bg === 0;
  }
  // colors
  getFgColor() {
    switch (this.fg & 50331648 /* CM_MASK */) {
      case 16777216 /* CM_P16 */:
      case 33554432 /* CM_P256 */:
        return this.fg & 255 /* PCOLOR_MASK */;
      case 50331648 /* CM_RGB */:
        return this.fg & 16777215 /* RGB_MASK */;
      default:
        return -1;
    }
  }
  getBgColor() {
    switch (this.bg & 50331648 /* CM_MASK */) {
      case 16777216 /* CM_P16 */:
      case 33554432 /* CM_P256 */:
        return this.bg & 255 /* PCOLOR_MASK */;
      case 50331648 /* CM_RGB */:
        return this.bg & 16777215 /* RGB_MASK */;
      default:
        return -1;
    }
  }
  // extended attrs
  hasExtendedAttrs() {
    return this.bg & 268435456 /* HAS_EXTENDED */;
  }
  updateExtended() {
    if (this.extended.isEmpty()) {
      this.bg &= ~268435456 /* HAS_EXTENDED */;
    } else {
      this.bg |= 268435456 /* HAS_EXTENDED */;
    }
  }
  getUnderlineColor() {
    if (this.bg & 268435456 /* HAS_EXTENDED */ && ~this.extended.underlineColor) {
      switch (this.extended.underlineColor & 50331648 /* CM_MASK */) {
        case 16777216 /* CM_P16 */:
        case 33554432 /* CM_P256 */:
          return this.extended.underlineColor & 255 /* PCOLOR_MASK */;
        case 50331648 /* CM_RGB */:
          return this.extended.underlineColor & 16777215 /* RGB_MASK */;
        default:
          return this.getFgColor();
      }
    }
    return this.getFgColor();
  }
  getUnderlineColorMode() {
    return this.bg & 268435456 /* HAS_EXTENDED */ && ~this.extended.underlineColor ? this.extended.underlineColor & 50331648 /* CM_MASK */ : this.getFgColorMode();
  }
  isUnderlineColorRGB() {
    return this.bg & 268435456 /* HAS_EXTENDED */ && ~this.extended.underlineColor ? (this.extended.underlineColor & 50331648 /* CM_MASK */) === 50331648 /* CM_RGB */ : this.isFgRGB();
  }
  isUnderlineColorPalette() {
    return this.bg & 268435456 /* HAS_EXTENDED */ && ~this.extended.underlineColor ? (this.extended.underlineColor & 50331648 /* CM_MASK */) === 16777216 /* CM_P16 */ || (this.extended.underlineColor & 50331648 /* CM_MASK */) === 33554432 /* CM_P256 */ : this.isFgPalette();
  }
  isUnderlineColorDefault() {
    return this.bg & 268435456 /* HAS_EXTENDED */ && ~this.extended.underlineColor ? (this.extended.underlineColor & 50331648 /* CM_MASK */) === 0 : this.isFgDefault();
  }
  getUnderlineStyle() {
    return this.fg & 268435456 /* UNDERLINE */ ? this.bg & 268435456 /* HAS_EXTENDED */ ? this.extended.underlineStyle : 1 /* SINGLE */ : 0 /* NONE */;
  }
  getUnderlineVariantOffset() {
    return this.extended.underlineVariantOffset;
  }
};
var ExtendedAttrs = class _ExtendedAttrs {
  constructor(ext = 0, urlId = 0) {
    this._ext = 0;
    this._urlId = 0;
    this._ext = ext;
    this._urlId = urlId;
  }
  get ext() {
    if (this._urlId) {
      return this._ext & ~469762048 /* UNDERLINE_STYLE */ | this.underlineStyle << 26;
    }
    return this._ext;
  }
  set ext(value) {
    this._ext = value;
  }
  get underlineStyle() {
    if (this._urlId) {
      return 5 /* DASHED */;
    }
    return (this._ext & 469762048 /* UNDERLINE_STYLE */) >> 26;
  }
  set underlineStyle(value) {
    this._ext &= ~469762048 /* UNDERLINE_STYLE */;
    this._ext |= value << 26 & 469762048 /* UNDERLINE_STYLE */;
  }
  get underlineColor() {
    return this._ext & (50331648 /* CM_MASK */ | 16777215 /* RGB_MASK */);
  }
  set underlineColor(value) {
    this._ext &= ~(50331648 /* CM_MASK */ | 16777215 /* RGB_MASK */);
    this._ext |= value & (50331648 /* CM_MASK */ | 16777215 /* RGB_MASK */);
  }
  get urlId() {
    return this._urlId;
  }
  set urlId(value) {
    this._urlId = value;
  }
  get underlineVariantOffset() {
    const val = (this._ext & 3758096384 /* VARIANT_OFFSET */) >> 29;
    if (val < 0) {
      return val ^ 4294967288;
    }
    return val;
  }
  set underlineVariantOffset(value) {
    this._ext &= ~3758096384 /* VARIANT_OFFSET */;
    this._ext |= value << 29 & 3758096384 /* VARIANT_OFFSET */;
  }
  clone() {
    return new _ExtendedAttrs(this._ext, this._urlId);
  }
  /**
   * Convenient method to indicate whether the object holds no additional information,
   * that needs to be persistant in the buffer.
   */
  isEmpty() {
    return this.underlineStyle === 0 /* NONE */ && this._urlId === 0;
  }
};

// node_modules/@xterm/xterm/src/vs/base/common/linkedList.ts
var _Node = class _Node {
  constructor(element) {
    this.element = element;
    this.next = _Node.Undefined;
    this.prev = _Node.Undefined;
  }
};
_Node.Undefined = new _Node(void 0);
var Node2 = _Node;

// node_modules/@xterm/xterm/src/vs/base/common/stopwatch.ts
var hasPerformanceNow = globalThis.performance && typeof globalThis.performance.now === "function";
var StopWatch = class _StopWatch {
  static create(highResolution) {
    return new _StopWatch(highResolution);
  }
  constructor(highResolution) {
    this._now = hasPerformanceNow && highResolution === false ? Date.now : globalThis.performance.now.bind(globalThis.performance);
    this._startTime = this._now();
    this._stopTime = -1;
  }
  stop() {
    this._stopTime = this._now();
  }
  reset() {
    this._startTime = this._now();
    this._stopTime = -1;
  }
  elapsed() {
    if (this._stopTime !== -1) {
      return this._stopTime - this._startTime;
    }
    return this._now() - this._startTime;
  }
};

// node_modules/@xterm/xterm/src/vs/base/common/event.ts
var _enableListenerGCedWarning = false;
var _enableDisposeWithListenerWarning = false;
var _enableSnapshotPotentialLeakWarning = false;
var Event;
((Event2) => {
  Event2.None = () => Disposable.None;
  function _addLeakageTraceLogic(options) {
    if (_enableSnapshotPotentialLeakWarning) {
      const { onDidAddListener: origListenerDidAdd } = options;
      const stack = Stacktrace.create();
      let count = 0;
      options.onDidAddListener = () => {
        if (++count === 2) {
          console.warn("snapshotted emitter LIKELY used public and SHOULD HAVE BEEN created with DisposableStore. snapshotted here");
          stack.print();
        }
        origListenerDidAdd?.();
      };
    }
  }
  function defer(event, disposable) {
    return debounce(event, () => void 0, 0, void 0, true, void 0, disposable);
  }
  Event2.defer = defer;
  function once(event) {
    return (listener, thisArgs = null, disposables) => {
      let didFire = false;
      let result = void 0;
      result = event((e) => {
        if (didFire) {
          return;
        } else if (result) {
          result.dispose();
        } else {
          didFire = true;
        }
        return listener.call(thisArgs, e);
      }, null, disposables);
      if (didFire) {
        result.dispose();
      }
      return result;
    };
  }
  Event2.once = once;
  function map(event, map2, disposable) {
    return snapshot((listener, thisArgs = null, disposables) => event((i) => listener.call(thisArgs, map2(i)), null, disposables), disposable);
  }
  Event2.map = map;
  function forEach(event, each, disposable) {
    return snapshot((listener, thisArgs = null, disposables) => event((i) => {
      each(i);
      listener.call(thisArgs, i);
    }, null, disposables), disposable);
  }
  Event2.forEach = forEach;
  function filter(event, filter2, disposable) {
    return snapshot((listener, thisArgs = null, disposables) => event((e) => filter2(e) && listener.call(thisArgs, e), null, disposables), disposable);
  }
  Event2.filter = filter;
  function signal(event) {
    return event;
  }
  Event2.signal = signal;
  function any(...events) {
    return (listener, thisArgs = null, disposables) => {
      const disposable = combinedDisposable(...events.map((event) => event((e) => listener.call(thisArgs, e))));
      return addAndReturnDisposable(disposable, disposables);
    };
  }
  Event2.any = any;
  function reduce(event, merge, initial, disposable) {
    let output = initial;
    return map(event, (e) => {
      output = merge(output, e);
      return output;
    }, disposable);
  }
  Event2.reduce = reduce;
  function snapshot(event, disposable) {
    let listener;
    const options = {
      onWillAddFirstListener() {
        listener = event(emitter.fire, emitter);
      },
      onDidRemoveLastListener() {
        listener?.dispose();
      }
    };
    if (!disposable) {
      _addLeakageTraceLogic(options);
    }
    const emitter = new Emitter(options);
    disposable?.add(emitter);
    return emitter.event;
  }
  function addAndReturnDisposable(d, store) {
    if (store instanceof Array) {
      store.push(d);
    } else if (store) {
      store.add(d);
    }
    return d;
  }
  function debounce(event, merge, delay = 100, leading = false, flushOnListenerRemove = false, leakWarningThreshold, disposable) {
    let subscription;
    let output = void 0;
    let handle = void 0;
    let numDebouncedCalls = 0;
    let doFire;
    const options = {
      leakWarningThreshold,
      onWillAddFirstListener() {
        subscription = event((cur) => {
          numDebouncedCalls++;
          output = merge(output, cur);
          if (leading && !handle) {
            emitter.fire(output);
            output = void 0;
          }
          doFire = () => {
            const _output = output;
            output = void 0;
            handle = void 0;
            if (!leading || numDebouncedCalls > 1) {
              emitter.fire(_output);
            }
            numDebouncedCalls = 0;
          };
          if (typeof delay === "number") {
            clearTimeout(handle);
            handle = setTimeout(doFire, delay);
          } else {
            if (handle === void 0) {
              handle = 0;
              queueMicrotask(doFire);
            }
          }
        });
      },
      onWillRemoveListener() {
        if (flushOnListenerRemove && numDebouncedCalls > 0) {
          doFire?.();
        }
      },
      onDidRemoveLastListener() {
        doFire = void 0;
        subscription.dispose();
      }
    };
    if (!disposable) {
      _addLeakageTraceLogic(options);
    }
    const emitter = new Emitter(options);
    disposable?.add(emitter);
    return emitter.event;
  }
  Event2.debounce = debounce;
  function accumulate(event, delay = 0, disposable) {
    return Event2.debounce(event, (last, e) => {
      if (!last) {
        return [e];
      }
      last.push(e);
      return last;
    }, delay, void 0, true, void 0, disposable);
  }
  Event2.accumulate = accumulate;
  function latch(event, equals = (a, b) => a === b, disposable) {
    let firstCall = true;
    let cache;
    return filter(event, (value) => {
      const shouldEmit = firstCall || !equals(value, cache);
      firstCall = false;
      cache = value;
      return shouldEmit;
    }, disposable);
  }
  Event2.latch = latch;
  function split(event, isT, disposable) {
    return [
      Event2.filter(event, isT, disposable),
      Event2.filter(event, (e) => !isT(e), disposable)
    ];
  }
  Event2.split = split;
  function buffer(event, flushAfterTimeout = false, _buffer = [], disposable) {
    let buffer2 = _buffer.slice();
    let listener = event((e) => {
      if (buffer2) {
        buffer2.push(e);
      } else {
        emitter.fire(e);
      }
    });
    if (disposable) {
      disposable.add(listener);
    }
    const flush = () => {
      buffer2?.forEach((e) => emitter.fire(e));
      buffer2 = null;
    };
    const emitter = new Emitter({
      onWillAddFirstListener() {
        if (!listener) {
          listener = event((e) => emitter.fire(e));
          if (disposable) {
            disposable.add(listener);
          }
        }
      },
      onDidAddFirstListener() {
        if (buffer2) {
          if (flushAfterTimeout) {
            setTimeout(flush);
          } else {
            flush();
          }
        }
      },
      onDidRemoveLastListener() {
        if (listener) {
          listener.dispose();
        }
        listener = null;
      }
    });
    if (disposable) {
      disposable.add(emitter);
    }
    return emitter.event;
  }
  Event2.buffer = buffer;
  function chain(event, sythensize) {
    const fn = (listener, thisArgs, disposables) => {
      const cs = sythensize(new ChainableSynthesis());
      return event(function(value) {
        const result = cs.evaluate(value);
        if (result !== HaltChainable) {
          listener.call(thisArgs, result);
        }
      }, void 0, disposables);
    };
    return fn;
  }
  Event2.chain = chain;
  const HaltChainable = Symbol("HaltChainable");
  class ChainableSynthesis {
    constructor() {
      this.steps = [];
    }
    map(fn) {
      this.steps.push(fn);
      return this;
    }
    forEach(fn) {
      this.steps.push((v) => {
        fn(v);
        return v;
      });
      return this;
    }
    filter(fn) {
      this.steps.push((v) => fn(v) ? v : HaltChainable);
      return this;
    }
    reduce(merge, initial) {
      let last = initial;
      this.steps.push((v) => {
        last = merge(last, v);
        return last;
      });
      return this;
    }
    latch(equals = (a, b) => a === b) {
      let firstCall = true;
      let cache;
      this.steps.push((value) => {
        const shouldEmit = firstCall || !equals(value, cache);
        firstCall = false;
        cache = value;
        return shouldEmit ? value : HaltChainable;
      });
      return this;
    }
    evaluate(value) {
      for (const step of this.steps) {
        value = step(value);
        if (value === HaltChainable) {
          break;
        }
      }
      return value;
    }
  }
  function fromNodeEventEmitter(emitter, eventName, map2 = (id2) => id2) {
    const fn = (...args) => result.fire(map2(...args));
    const onFirstListenerAdd = () => emitter.on(eventName, fn);
    const onLastListenerRemove = () => emitter.removeListener(eventName, fn);
    const result = new Emitter({ onWillAddFirstListener: onFirstListenerAdd, onDidRemoveLastListener: onLastListenerRemove });
    return result.event;
  }
  Event2.fromNodeEventEmitter = fromNodeEventEmitter;
  function fromDOMEventEmitter(emitter, eventName, map2 = (id2) => id2) {
    const fn = (...args) => result.fire(map2(...args));
    const onFirstListenerAdd = () => emitter.addEventListener(eventName, fn);
    const onLastListenerRemove = () => emitter.removeEventListener(eventName, fn);
    const result = new Emitter({ onWillAddFirstListener: onFirstListenerAdd, onDidRemoveLastListener: onLastListenerRemove });
    return result.event;
  }
  Event2.fromDOMEventEmitter = fromDOMEventEmitter;
  function toPromise(event) {
    return new Promise((resolve) => once(event)(resolve));
  }
  Event2.toPromise = toPromise;
  function fromPromise(promise) {
    const result = new Emitter();
    promise.then((res) => {
      result.fire(res);
    }, () => {
      result.fire(void 0);
    }).finally(() => {
      result.dispose();
    });
    return result.event;
  }
  Event2.fromPromise = fromPromise;
  function forward(from, to) {
    return from((e) => to.fire(e));
  }
  Event2.forward = forward;
  function runAndSubscribe(event, handler, initial) {
    handler(initial);
    return event((e) => handler(e));
  }
  Event2.runAndSubscribe = runAndSubscribe;
  class EmitterObserver {
    constructor(_observable, store) {
      this._observable = _observable;
      this._counter = 0;
      this._hasChanged = false;
      const options = {
        onWillAddFirstListener: () => {
          _observable.addObserver(this);
        },
        onDidRemoveLastListener: () => {
          _observable.removeObserver(this);
        }
      };
      if (!store) {
        _addLeakageTraceLogic(options);
      }
      this.emitter = new Emitter(options);
      if (store) {
        store.add(this.emitter);
      }
    }
    beginUpdate(_observable) {
      this._counter++;
    }
    handlePossibleChange(_observable) {
    }
    handleChange(_observable, _change) {
      this._hasChanged = true;
    }
    endUpdate(_observable) {
      this._counter--;
      if (this._counter === 0) {
        this._observable.reportChanges();
        if (this._hasChanged) {
          this._hasChanged = false;
          this.emitter.fire(this._observable.get());
        }
      }
    }
  }
  function fromObservable(obs, store) {
    const observer = new EmitterObserver(obs, store);
    return observer.emitter.event;
  }
  Event2.fromObservable = fromObservable;
  function fromObservableLight(observable) {
    return (listener, thisArgs, disposables) => {
      let count = 0;
      let didChange = false;
      const observer = {
        beginUpdate() {
          count++;
        },
        endUpdate() {
          count--;
          if (count === 0) {
            observable.reportChanges();
            if (didChange) {
              didChange = false;
              listener.call(thisArgs);
            }
          }
        },
        handlePossibleChange() {
        },
        handleChange() {
          didChange = true;
        }
      };
      observable.addObserver(observer);
      observable.reportChanges();
      const disposable = {
        dispose() {
          observable.removeObserver(observer);
        }
      };
      if (disposables instanceof DisposableStore) {
        disposables.add(disposable);
      } else if (Array.isArray(disposables)) {
        disposables.push(disposable);
      }
      return disposable;
    };
  }
  Event2.fromObservableLight = fromObservableLight;
})(Event || (Event = {}));
var _EventProfiling = class _EventProfiling {
  constructor(name) {
    this.listenerCount = 0;
    this.invocationCount = 0;
    this.elapsedOverall = 0;
    this.durations = [];
    this.name = `${name}_${_EventProfiling._idPool++}`;
    _EventProfiling.all.add(this);
  }
  start(listenerCount) {
    this._stopWatch = new StopWatch();
    this.listenerCount = listenerCount;
  }
  stop() {
    if (this._stopWatch) {
      const elapsed = this._stopWatch.elapsed();
      this.durations.push(elapsed);
      this.elapsedOverall += elapsed;
      this.invocationCount += 1;
      this._stopWatch = void 0;
    }
  }
};
_EventProfiling.all = /* @__PURE__ */ new Set();
_EventProfiling._idPool = 0;
var EventProfiling = _EventProfiling;
var _globalLeakWarningThreshold = -1;
var _LeakageMonitor = class _LeakageMonitor {
  constructor(_errorHandler, threshold, name = (_LeakageMonitor._idPool++).toString(16).padStart(3, "0")) {
    this._errorHandler = _errorHandler;
    this.threshold = threshold;
    this.name = name;
    this._warnCountdown = 0;
  }
  dispose() {
    this._stacks?.clear();
  }
  check(stack, listenerCount) {
    const threshold = this.threshold;
    if (threshold <= 0 || listenerCount < threshold) {
      return void 0;
    }
    if (!this._stacks) {
      this._stacks = /* @__PURE__ */ new Map();
    }
    const count = this._stacks.get(stack.value) || 0;
    this._stacks.set(stack.value, count + 1);
    this._warnCountdown -= 1;
    if (this._warnCountdown <= 0) {
      this._warnCountdown = threshold * 0.5;
      const [topStack, topCount] = this.getMostFrequentStack();
      const message = `[${this.name}] potential listener LEAK detected, having ${listenerCount} listeners already. MOST frequent listener (${topCount}):`;
      console.warn(message);
      console.warn(topStack);
      const error = new ListenerLeakError(message, topStack);
      this._errorHandler(error);
    }
    return () => {
      const count2 = this._stacks.get(stack.value) || 0;
      this._stacks.set(stack.value, count2 - 1);
    };
  }
  getMostFrequentStack() {
    if (!this._stacks) {
      return void 0;
    }
    let topStack;
    let topCount = 0;
    for (const [stack, count] of this._stacks) {
      if (!topStack || topCount < count) {
        topStack = [stack, count];
        topCount = count;
      }
    }
    return topStack;
  }
};
_LeakageMonitor._idPool = 1;
var LeakageMonitor = _LeakageMonitor;
var Stacktrace = class _Stacktrace {
  constructor(value) {
    this.value = value;
  }
  static create() {
    const err = new Error();
    return new _Stacktrace(err.stack ?? "");
  }
  print() {
    console.warn(this.value.split("\n").slice(2).join("\n"));
  }
};
var ListenerLeakError = class extends Error {
  constructor(message, stack) {
    super(message);
    this.name = "ListenerLeakError";
    this.stack = stack;
  }
};
var ListenerRefusalError = class extends Error {
  constructor(message, stack) {
    super(message);
    this.name = "ListenerRefusalError";
    this.stack = stack;
  }
};
var id = 0;
var UniqueContainer = class {
  constructor(value) {
    this.value = value;
    this.id = id++;
  }
};
var compactionThreshold = 2;
var forEachListener = (listeners, fn) => {
  if (listeners instanceof UniqueContainer) {
    fn(listeners);
  } else {
    for (let i = 0; i < listeners.length; i++) {
      const l = listeners[i];
      if (l) {
        fn(l);
      }
    }
  }
};
var _listenerFinalizers;
if (_enableListenerGCedWarning) {
  const leaks = [];
  setInterval(() => {
    if (leaks.length === 0) {
      return;
    }
    console.warn("[LEAKING LISTENERS] GC'ed these listeners that were NOT yet disposed:");
    console.warn(leaks.join("\n"));
    leaks.length = 0;
  }, 3e3);
  _listenerFinalizers = new FinalizationRegistry((heldValue) => {
    if (typeof heldValue === "string") {
      leaks.push(heldValue);
    }
  });
}
var Emitter = class {
  constructor(options) {
    this._size = 0;
    this._options = options;
    this._leakageMon = _globalLeakWarningThreshold > 0 || this._options?.leakWarningThreshold ? new LeakageMonitor(options?.onListenerError ?? onUnexpectedError, this._options?.leakWarningThreshold ?? _globalLeakWarningThreshold) : void 0;
    this._perfMon = this._options?._profName ? new EventProfiling(this._options._profName) : void 0;
    this._deliveryQueue = this._options?.deliveryQueue;
  }
  dispose() {
    if (!this._disposed) {
      this._disposed = true;
      if (this._deliveryQueue?.current === this) {
        this._deliveryQueue.reset();
      }
      if (this._listeners) {
        if (_enableDisposeWithListenerWarning) {
          const listeners = this._listeners;
          queueMicrotask(() => {
            forEachListener(listeners, (l) => l.stack?.print());
          });
        }
        this._listeners = void 0;
        this._size = 0;
      }
      this._options?.onDidRemoveLastListener?.();
      this._leakageMon?.dispose();
    }
  }
  /**
   * For the public to allow to subscribe
   * to events from this Emitter
   */
  get event() {
    this._event ??= (callback, thisArgs, disposables) => {
      if (this._leakageMon && this._size > this._leakageMon.threshold ** 2) {
        const message = `[${this._leakageMon.name}] REFUSES to accept new listeners because it exceeded its threshold by far (${this._size} vs ${this._leakageMon.threshold})`;
        console.warn(message);
        const tuple = this._leakageMon.getMostFrequentStack() ?? ["UNKNOWN stack", -1];
        const error = new ListenerRefusalError(`${message}. HINT: Stack shows most frequent listener (${tuple[1]}-times)`, tuple[0]);
        const errorHandler2 = this._options?.onListenerError || onUnexpectedError;
        errorHandler2(error);
        return Disposable.None;
      }
      if (this._disposed) {
        return Disposable.None;
      }
      if (thisArgs) {
        callback = callback.bind(thisArgs);
      }
      const contained = new UniqueContainer(callback);
      let removeMonitor;
      let stack;
      if (this._leakageMon && this._size >= Math.ceil(this._leakageMon.threshold * 0.2)) {
        contained.stack = Stacktrace.create();
        removeMonitor = this._leakageMon.check(contained.stack, this._size + 1);
      }
      if (_enableDisposeWithListenerWarning) {
        contained.stack = stack ?? Stacktrace.create();
      }
      if (!this._listeners) {
        this._options?.onWillAddFirstListener?.(this);
        this._listeners = contained;
        this._options?.onDidAddFirstListener?.(this);
      } else if (this._listeners instanceof UniqueContainer) {
        this._deliveryQueue ??= new EventDeliveryQueuePrivate();
        this._listeners = [this._listeners, contained];
      } else {
        this._listeners.push(contained);
      }
      this._size++;
      const result = toDisposable(() => {
        _listenerFinalizers?.unregister(result);
        removeMonitor?.();
        this._removeListener(contained);
      });
      if (disposables instanceof DisposableStore) {
        disposables.add(result);
      } else if (Array.isArray(disposables)) {
        disposables.push(result);
      }
      if (_listenerFinalizers) {
        const stack2 = new Error().stack.split("\n").slice(2, 3).join("\n").trim();
        const match = /(file:|vscode-file:\/\/vscode-app)?(\/[^:]*:\d+:\d+)/.exec(stack2);
        _listenerFinalizers.register(result, match?.[2] ?? stack2, result);
      }
      return result;
    };
    return this._event;
  }
  _removeListener(listener) {
    this._options?.onWillRemoveListener?.(this);
    if (!this._listeners) {
      return;
    }
    if (this._size === 1) {
      this._listeners = void 0;
      this._options?.onDidRemoveLastListener?.(this);
      this._size = 0;
      return;
    }
    const listeners = this._listeners;
    const index = listeners.indexOf(listener);
    if (index === -1) {
      console.log("disposed?", this._disposed);
      console.log("size?", this._size);
      console.log("arr?", JSON.stringify(this._listeners));
      throw new Error("Attempted to dispose unknown listener");
    }
    this._size--;
    listeners[index] = void 0;
    const adjustDeliveryQueue = this._deliveryQueue.current === this;
    if (this._size * compactionThreshold <= listeners.length) {
      let n = 0;
      for (let i = 0; i < listeners.length; i++) {
        if (listeners[i]) {
          listeners[n++] = listeners[i];
        } else if (adjustDeliveryQueue) {
          this._deliveryQueue.end--;
          if (n < this._deliveryQueue.i) {
            this._deliveryQueue.i--;
          }
        }
      }
      listeners.length = n;
    }
  }
  _deliver(listener, value) {
    if (!listener) {
      return;
    }
    const errorHandler2 = this._options?.onListenerError || onUnexpectedError;
    if (!errorHandler2) {
      listener.value(value);
      return;
    }
    try {
      listener.value(value);
    } catch (e) {
      errorHandler2(e);
    }
  }
  /** Delivers items in the queue. Assumes the queue is ready to go. */
  _deliverQueue(dq) {
    const listeners = dq.current._listeners;
    while (dq.i < dq.end) {
      this._deliver(listeners[dq.i++], dq.value);
    }
    dq.reset();
  }
  /**
   * To be kept private to fire an event to
   * subscribers
   */
  fire(event) {
    if (this._deliveryQueue?.current) {
      this._deliverQueue(this._deliveryQueue);
      this._perfMon?.stop();
    }
    this._perfMon?.start(this._size);
    if (!this._listeners) {
    } else if (this._listeners instanceof UniqueContainer) {
      this._deliver(this._listeners, event);
    } else {
      const dq = this._deliveryQueue;
      dq.enqueue(this, event, this._listeners.length);
      this._deliverQueue(dq);
    }
    this._perfMon?.stop();
  }
  hasListeners() {
    return this._size > 0;
  }
};
var EventDeliveryQueuePrivate = class {
  constructor() {
    /**
     * Index in current's listener list.
     */
    this.i = -1;
    /**
     * The last index in the listener's list to deliver.
     */
    this.end = 0;
  }
  enqueue(emitter, value, end) {
    this.i = 0;
    this.end = end;
    this.current = emitter;
    this.value = value;
  }
  reset() {
    this.i = this.end;
    this.current = void 0;
    this.value = void 0;
  }
};

// vendor/xterm-addon-webgl/src/TextureAtlas.ts
var NULL_RASTERIZED_GLYPH = {
  texturePage: 0,
  texturePosition: { x: 0, y: 0 },
  texturePositionClipSpace: { x: 0, y: 0 },
  offset: { x: 0, y: 0 },
  size: { x: 0, y: 0 },
  sizeClipSpace: { x: 0, y: 0 }
};
var TMP_CANVAS_GLYPH_PADDING = 2;
var $glyph = void 0;
var TextureAtlas = class _TextureAtlas {
  constructor(_document, _config, _unicodeService) {
    this._document = _document;
    this._config = _config;
    this._unicodeService = _unicodeService;
    this._didWarmUp = false;
    this._cacheMap = new FourKeyMap();
    this._cacheMapCombined = new FourKeyMap();
    // The texture that the atlas is drawn to
    this._pages = [];
    // The set of atlas pages that can be written to
    this._activePages = [];
    this._workBoundingBox = { top: 0, left: 0, bottom: 0, right: 0 };
    this._workAttributeData = new AttributeData();
    this._textureSize = 512;
    this._onAddTextureAtlasCanvas = new Emitter();
    this.onAddTextureAtlasCanvas = this._onAddTextureAtlasCanvas.event;
    this._onRemoveTextureAtlasCanvas = new Emitter();
    this.onRemoveTextureAtlasCanvas = this._onRemoveTextureAtlasCanvas.event;
    /**
     * Incremented whenever cached glyph texture page mappings may be stale (page merges, overflow
     * page creation, clears, evictions). Every renderer sharing this atlas compares it against its
     * own last-seen value and rebuilds its model when it changed — a consume-once flag would only
     * reach the first renderer that draws (xterm.js #6042/#6055).
     */
    this._pageLayoutVersion = 0;
    this._createNewPage();
    this._tmpCanvas = createCanvas(
      _document,
      this._config.deviceCellWidth * 4 + TMP_CANVAS_GLYPH_PADDING * 2,
      this._config.deviceCellHeight + TMP_CANVAS_GLYPH_PADDING * 2
    );
    this._tmpCtx = throwIfFalsy(this._tmpCanvas.getContext("2d", {
      alpha: this._config.allowTransparency,
      willReadFrequently: true
    }));
  }
  get pages() {
    return this._pages;
  }
  dispose() {
    this._tmpCanvas.remove();
    for (const page of this.pages) {
      page.canvas.remove();
    }
    this._onAddTextureAtlasCanvas.dispose();
    this._onRemoveTextureAtlasCanvas.dispose();
  }
  warmUp() {
    if (!this._didWarmUp) {
      this._doWarmUp();
      this._didWarmUp = true;
    }
  }
  _doWarmUp() {
    const queue = new IdleTaskQueue();
    for (let i = 33; i < 126; i++) {
      queue.enqueue(() => {
        if (!this._cacheMap.get(i, DEFAULT_COLOR, DEFAULT_COLOR, DEFAULT_EXT)) {
          const rasterizedGlyph = this._drawToCache(i, DEFAULT_COLOR, DEFAULT_COLOR, DEFAULT_EXT, false, void 0);
          this._cacheMap.set(i, DEFAULT_COLOR, DEFAULT_COLOR, DEFAULT_EXT, rasterizedGlyph);
        }
      });
    }
  }
  get pageLayoutVersion() {
    return this._pageLayoutVersion;
  }
  clearTexture() {
    if (this._pages.length === 0 || this._pages[0].currentRow.x === 0 && this._pages[0].currentRow.y === 0) {
      return;
    }
    for (const page of this._pages) {
      page.clear();
    }
    this._cacheMap.clear();
    this._cacheMapCombined.clear();
    this._didWarmUp = false;
    this._pageLayoutVersion++;
  }
  _createNewPage() {
    if (_TextureAtlas.maxAtlasPages && this._pages.length >= Math.max(4, _TextureAtlas.maxAtlasPages)) {
      const pagesBySize = this._pages.filter((e) => {
        return e.canvas.width * 2 <= (_TextureAtlas.maxTextureSize || 4096 /* FORCED_MAX_TEXTURE_SIZE */);
      }).sort((a, b) => {
        if (b.canvas.width !== a.canvas.width) {
          return b.canvas.width - a.canvas.width;
        }
        return b.percentageUsed - a.percentageUsed;
      });
      let sameSizeI = -1;
      let size = 0;
      for (let i = 0; i < pagesBySize.length; i++) {
        if (pagesBySize[i].canvas.width !== size) {
          sameSizeI = i;
          size = pagesBySize[i].canvas.width;
        } else if (i - sameSizeI === 3) {
          break;
        }
      }
      const mergingPages = pagesBySize.slice(sameSizeI, sameSizeI + 4);
      if (mergingPages.length < 4 || mergingPages.some((p) => p.canvas.width !== mergingPages[0].canvas.width) || mergingPages.some((p) => p.glyphs.length === 0)) {
        this._evictAllPages();
        const newPage2 = new AtlasPage(this._document, this._textureSize);
        this._pages.push(newPage2);
        this._activePages.push(newPage2);
        this._onAddTextureAtlasCanvas.fire(newPage2.canvas);
        return newPage2;
      }
      const sortedMergingPagesIndexes = mergingPages.map((e) => e.glyphs[0].texturePage).sort((a, b) => a > b ? 1 : -1);
      const mergedPageIndex = this.pages.length - mergingPages.length;
      const mergedPage = this._mergePages(mergingPages, mergedPageIndex);
      mergedPage.version = ++AtlasPage.nextVersion;
      for (let i = sortedMergingPagesIndexes.length - 1; i >= 0; i--) {
        this._deletePage(sortedMergingPagesIndexes[i]);
      }
      this.pages.push(mergedPage);
      this._pageLayoutVersion++;
      this._onAddTextureAtlasCanvas.fire(mergedPage.canvas);
    }
    const newPage = new AtlasPage(this._document, this._textureSize);
    this._pages.push(newPage);
    this._activePages.push(newPage);
    this._onAddTextureAtlasCanvas.fire(newPage.canvas);
    return newPage;
  }
  _mergePages(mergingPages, mergedPageIndex) {
    const mergedSize = mergingPages[0].canvas.width * 2;
    const mergedPage = new AtlasPage(this._document, mergedSize, mergingPages);
    for (const [i, p] of mergingPages.entries()) {
      const xOffset = i * p.canvas.width % mergedSize;
      const yOffset = Math.floor(i / 2) * p.canvas.height;
      mergedPage.ctx.drawImage(p.canvas, xOffset, yOffset);
      for (const g of p.glyphs) {
        g.texturePage = mergedPageIndex;
        g.sizeClipSpace.x = g.size.x / mergedSize;
        g.sizeClipSpace.y = g.size.y / mergedSize;
        g.texturePosition.x += xOffset;
        g.texturePosition.y += yOffset;
        g.texturePositionClipSpace.x = g.texturePosition.x / mergedSize;
        g.texturePositionClipSpace.y = g.texturePosition.y / mergedSize;
      }
      this._onRemoveTextureAtlasCanvas.fire(p.canvas);
      const index = this._activePages.indexOf(p);
      if (index !== -1) {
        this._activePages.splice(index, 1);
      }
    }
    return mergedPage;
  }
  _deletePage(pageIndex) {
    this._pages.splice(pageIndex, 1);
    for (let j = pageIndex; j < this._pages.length; j++) {
      const adjustingPage = this._pages[j];
      for (const g of adjustingPage.glyphs) {
        g.texturePage--;
      }
      adjustingPage.version = ++AtlasPage.nextVersion;
    }
  }
  /**
   * Drops every page and cached glyph. Used instead of growing past the renderer's texture
   * capacity (maxAtlasPages) when pages can no longer be merged; glyphs are rasterized again on
   * demand (xterm.js #6043).
   */
  _evictAllPages() {
    for (const page of this._pages) {
      this._onRemoveTextureAtlasCanvas.fire(page.canvas);
      page.canvas.remove();
    }
    this._pages.length = 0;
    this._activePages.length = 0;
    this._overflowSizePage = void 0;
    this._cacheMap.clear();
    this._cacheMapCombined.clear();
    this._didWarmUp = false;
    this._pageLayoutVersion++;
  }
  getRasterizedGlyphCombinedChar(chars, bg, fg, ext, restrictToCellHeight, domContainer) {
    return this._getFromCacheMap(this._cacheMapCombined, chars, bg, fg, ext, restrictToCellHeight, domContainer);
  }
  getRasterizedGlyph(code, bg, fg, ext, restrictToCellHeight, domContainer) {
    return this._getFromCacheMap(this._cacheMap, code, bg, fg, ext, restrictToCellHeight, domContainer);
  }
  /**
   * Gets the glyphs texture coords, drawing the texture if it's not already
   */
  _getFromCacheMap(cacheMap, key, bg, fg, ext, restrictToCellHeight, domContainer) {
    $glyph = cacheMap.get(key, bg, fg, ext);
    if (!$glyph) {
      $glyph = this._drawToCache(key, bg, fg, ext, restrictToCellHeight, domContainer);
      cacheMap.set(key, bg, fg, ext, $glyph);
    }
    return $glyph;
  }
  _getColorFromAnsiIndex(idx) {
    if (idx >= this._config.colors.ansi.length) {
      throw new Error("No color found for idx " + idx);
    }
    return this._config.colors.ansi[idx];
  }
  _getBackgroundColor(bgColorMode, bgColor, inverse, dim) {
    if (this._config.allowTransparency) {
      return NULL_COLOR;
    }
    let result;
    switch (bgColorMode) {
      case 16777216 /* CM_P16 */:
      case 33554432 /* CM_P256 */:
        result = this._getColorFromAnsiIndex(bgColor);
        break;
      case 50331648 /* CM_RGB */:
        const arr = AttributeData.toColorRGB(bgColor);
        result = channels.toColor(arr[0], arr[1], arr[2]);
        break;
      case 0 /* CM_DEFAULT */:
      default:
        if (inverse) {
          result = color.opaque(this._config.colors.foreground);
        } else {
          result = this._config.colors.background;
        }
        break;
    }
    if (!this._config.allowTransparency) {
      result = color.opaque(result);
    }
    return result;
  }
  _getForegroundColor(bg, bgColorMode, bgColor, fg, fgColorMode, fgColor, inverse, dim, bold, excludeFromContrastRatioDemands) {
    const minimumContrastColor = this._getMinimumContrastColor(bg, bgColorMode, bgColor, fg, fgColorMode, fgColor, inverse, bold, dim, excludeFromContrastRatioDemands);
    if (minimumContrastColor) {
      return minimumContrastColor;
    }
    let result;
    switch (fgColorMode) {
      case 16777216 /* CM_P16 */:
      case 33554432 /* CM_P256 */:
        if (this._config.drawBoldTextInBrightColors && bold && fgColor < 8) {
          fgColor += 8;
        }
        result = this._getColorFromAnsiIndex(fgColor);
        break;
      case 50331648 /* CM_RGB */:
        const arr = AttributeData.toColorRGB(fgColor);
        result = channels.toColor(arr[0], arr[1], arr[2]);
        break;
      case 0 /* CM_DEFAULT */:
      default:
        if (inverse) {
          result = this._config.colors.background;
        } else {
          result = this._config.colors.foreground;
        }
    }
    if (this._config.allowTransparency) {
      result = color.opaque(result);
    }
    if (dim) {
      result = color.multiplyOpacity(result, DIM_OPACITY);
    }
    return result;
  }
  _resolveBackgroundRgba(bgColorMode, bgColor, inverse) {
    switch (bgColorMode) {
      case 16777216 /* CM_P16 */:
      case 33554432 /* CM_P256 */:
        return this._getColorFromAnsiIndex(bgColor).rgba;
      case 50331648 /* CM_RGB */:
        return bgColor << 8;
      case 0 /* CM_DEFAULT */:
      default:
        if (inverse) {
          return this._config.colors.foreground.rgba;
        }
        return this._config.colors.background.rgba;
    }
  }
  _resolveForegroundRgba(fgColorMode, fgColor, inverse, bold) {
    switch (fgColorMode) {
      case 16777216 /* CM_P16 */:
      case 33554432 /* CM_P256 */:
        if (this._config.drawBoldTextInBrightColors && bold && fgColor < 8) {
          fgColor += 8;
        }
        return this._getColorFromAnsiIndex(fgColor).rgba;
      case 50331648 /* CM_RGB */:
        return fgColor << 8;
      case 0 /* CM_DEFAULT */:
      default:
        if (inverse) {
          return this._config.colors.background.rgba;
        }
        return this._config.colors.foreground.rgba;
    }
  }
  _getMinimumContrastColor(bg, bgColorMode, bgColor, fg, fgColorMode, fgColor, inverse, bold, dim, excludeFromContrastRatioDemands) {
    if (this._config.minimumContrastRatio === 1 || excludeFromContrastRatioDemands) {
      return void 0;
    }
    const cache = this._getContrastCache(dim);
    const adjustedColor = cache.getColor(bg, fg);
    if (adjustedColor !== void 0) {
      return adjustedColor || void 0;
    }
    const bgRgba = this._resolveBackgroundRgba(bgColorMode, bgColor, inverse);
    const fgRgba = this._resolveForegroundRgba(fgColorMode, fgColor, inverse, bold);
    const result = rgba.ensureContrastRatio(bgRgba, fgRgba, this._config.minimumContrastRatio / (dim ? 2 : 1));
    if (!result) {
      cache.setColor(bg, fg, null);
      return void 0;
    }
    const color2 = channels.toColor(
      result >> 24 & 255,
      result >> 16 & 255,
      result >> 8 & 255
    );
    cache.setColor(bg, fg, color2);
    return color2;
  }
  _getContrastCache(dim) {
    if (dim) {
      return this._config.colors.halfContrastCache;
    }
    return this._config.colors.contrastCache;
  }
  _drawToCache(codeOrChars, bg, fg, ext, restrictToCellHeight, domContainer) {
    const chars = typeof codeOrChars === "number" ? String.fromCharCode(codeOrChars) : codeOrChars;
    if (domContainer && this._tmpCanvas.parentElement !== domContainer) {
      this._tmpCanvas.style.display = "none";
      domContainer.append(this._tmpCanvas);
    }
    const allowedWidth = Math.min(this._config.deviceCellWidth * Math.max(chars.length, 2) + TMP_CANVAS_GLYPH_PADDING * 2, this._config.deviceMaxTextureSize);
    if (this._tmpCanvas.width < allowedWidth) {
      this._tmpCanvas.width = allowedWidth;
    }
    const allowedHeight = Math.min(this._config.deviceCellHeight + TMP_CANVAS_GLYPH_PADDING * 4, this._textureSize);
    if (this._tmpCanvas.height < allowedHeight) {
      this._tmpCanvas.height = allowedHeight;
    }
    this._tmpCtx.save();
    this._workAttributeData.fg = fg;
    this._workAttributeData.bg = bg;
    this._workAttributeData.extended.ext = ext;
    const invisible = !!this._workAttributeData.isInvisible();
    if (invisible) {
      return NULL_RASTERIZED_GLYPH;
    }
    const bold = !!this._workAttributeData.isBold();
    const inverse = !!this._workAttributeData.isInverse();
    const dim = !!this._workAttributeData.isDim();
    const italic = !!this._workAttributeData.isItalic();
    const underline = !!this._workAttributeData.isUnderline();
    const strikethrough = !!this._workAttributeData.isStrikethrough();
    const overline = !!this._workAttributeData.isOverline();
    let fgColor = this._workAttributeData.getFgColor();
    let fgColorMode = this._workAttributeData.getFgColorMode();
    let bgColor = this._workAttributeData.getBgColor();
    let bgColorMode = this._workAttributeData.getBgColorMode();
    if (inverse) {
      const temp = fgColor;
      fgColor = bgColor;
      bgColor = temp;
      const temp2 = fgColorMode;
      fgColorMode = bgColorMode;
      bgColorMode = temp2;
    }
    const backgroundColor = this._getBackgroundColor(bgColorMode, bgColor, inverse, dim);
    this._tmpCtx.globalCompositeOperation = "copy";
    this._tmpCtx.fillStyle = backgroundColor.css;
    this._tmpCtx.fillRect(0, 0, this._tmpCanvas.width, this._tmpCanvas.height);
    this._tmpCtx.globalCompositeOperation = "source-over";
    const fontWeight = bold ? this._config.fontWeightBold : this._config.fontWeight;
    const fontStyle = italic ? "italic" : "";
    this._tmpCtx.font = `${fontStyle} ${fontWeight} ${this._config.fontSize * this._config.devicePixelRatio}px ${this._config.fontFamily}`;
    this._tmpCtx.textBaseline = TEXT_BASELINE;
    const powerlineGlyph = chars.length === 1 && isPowerlineGlyph(chars.charCodeAt(0));
    const restrictedPowerlineGlyph = chars.length === 1 && isRestrictedPowerlineGlyph(chars.charCodeAt(0));
    const foregroundColor = this._getForegroundColor(bg, bgColorMode, bgColor, fg, fgColorMode, fgColor, inverse, dim, bold, treatGlyphAsBackgroundColor(chars.charCodeAt(0)));
    this._tmpCtx.fillStyle = foregroundColor.css;
    const padding = restrictedPowerlineGlyph ? 0 : TMP_CANVAS_GLYPH_PADDING * 2;
    let customGlyph = false;
    if (this._config.customGlyphs !== false) {
      customGlyph = tryDrawCustomChar(this._tmpCtx, chars, padding, padding, this._config.deviceCellWidth, this._config.deviceCellHeight, this._config.fontSize, this._config.devicePixelRatio);
    }
    let enableClearThresholdCheck = !powerlineGlyph;
    let chWidth;
    if (typeof codeOrChars === "number") {
      chWidth = this._unicodeService.wcwidth(codeOrChars);
    } else {
      chWidth = this._unicodeService.getStringCellWidth(codeOrChars);
    }
    if (underline) {
      this._tmpCtx.save();
      const lineWidth = Math.max(1, Math.floor(this._config.fontSize * this._config.devicePixelRatio / 15));
      const yOffset = lineWidth % 2 === 1 ? 0.5 : 0;
      this._tmpCtx.lineWidth = lineWidth;
      if (this._workAttributeData.isUnderlineColorDefault()) {
        this._tmpCtx.strokeStyle = this._tmpCtx.fillStyle;
      } else if (this._workAttributeData.isUnderlineColorRGB()) {
        enableClearThresholdCheck = false;
        this._tmpCtx.strokeStyle = `rgb(${AttributeData.toColorRGB(this._workAttributeData.getUnderlineColor()).join(",")})`;
      } else {
        enableClearThresholdCheck = false;
        let fg2 = this._workAttributeData.getUnderlineColor();
        if (this._config.drawBoldTextInBrightColors && this._workAttributeData.isBold() && fg2 < 8) {
          fg2 += 8;
        }
        this._tmpCtx.strokeStyle = this._getColorFromAnsiIndex(fg2).css;
      }
      this._tmpCtx.beginPath();
      const xLeft = padding;
      const yTop = Math.ceil(padding + this._config.deviceCharHeight) - yOffset - (restrictToCellHeight ? lineWidth * 2 : 0);
      const yMid = yTop + lineWidth;
      const yBot = yTop + lineWidth * 2;
      let nextOffset = this._workAttributeData.getUnderlineVariantOffset();
      for (let i = 0; i < chWidth; i++) {
        this._tmpCtx.save();
        const xChLeft = xLeft + i * this._config.deviceCellWidth;
        const xChRight = xLeft + (i + 1) * this._config.deviceCellWidth;
        const xChMid = xChLeft + this._config.deviceCellWidth / 2;
        switch (this._workAttributeData.extended.underlineStyle) {
          case 2 /* DOUBLE */:
            this._tmpCtx.moveTo(xChLeft, yTop);
            this._tmpCtx.lineTo(xChRight, yTop);
            this._tmpCtx.moveTo(xChLeft, yBot);
            this._tmpCtx.lineTo(xChRight, yBot);
            break;
          case 3 /* CURLY */:
            const yCurlyBot = lineWidth <= 1 ? yBot : Math.ceil(padding + this._config.deviceCharHeight - lineWidth / 2) - yOffset;
            const yCurlyTop = lineWidth <= 1 ? yTop : Math.ceil(padding + this._config.deviceCharHeight + lineWidth / 2) - yOffset;
            const clipRegion = new Path2D();
            clipRegion.rect(xChLeft, yTop, this._config.deviceCellWidth, yBot - yTop);
            this._tmpCtx.clip(clipRegion);
            this._tmpCtx.moveTo(xChLeft - this._config.deviceCellWidth / 2, yMid);
            this._tmpCtx.bezierCurveTo(
              xChLeft - this._config.deviceCellWidth / 2,
              yCurlyTop,
              xChLeft,
              yCurlyTop,
              xChLeft,
              yMid
            );
            this._tmpCtx.bezierCurveTo(
              xChLeft,
              yCurlyBot,
              xChMid,
              yCurlyBot,
              xChMid,
              yMid
            );
            this._tmpCtx.bezierCurveTo(
              xChMid,
              yCurlyTop,
              xChRight,
              yCurlyTop,
              xChRight,
              yMid
            );
            this._tmpCtx.bezierCurveTo(
              xChRight,
              yCurlyBot,
              xChRight + this._config.deviceCellWidth / 2,
              yCurlyBot,
              xChRight + this._config.deviceCellWidth / 2,
              yMid
            );
            break;
          case 4 /* DOTTED */:
            const offsetWidth = nextOffset === 0 ? 0 : nextOffset >= lineWidth ? lineWidth * 2 - nextOffset : lineWidth - nextOffset;
            const isLineStart = nextOffset >= lineWidth ? false : true;
            if (isLineStart === false || offsetWidth === 0) {
              this._tmpCtx.setLineDash([Math.round(lineWidth), Math.round(lineWidth)]);
              this._tmpCtx.moveTo(xChLeft + offsetWidth, yTop);
              this._tmpCtx.lineTo(xChRight, yTop);
            } else {
              this._tmpCtx.setLineDash([Math.round(lineWidth), Math.round(lineWidth)]);
              this._tmpCtx.moveTo(xChLeft, yTop);
              this._tmpCtx.lineTo(xChLeft + offsetWidth, yTop);
              this._tmpCtx.moveTo(xChLeft + offsetWidth + lineWidth, yTop);
              this._tmpCtx.lineTo(xChRight, yTop);
            }
            nextOffset = computeNextVariantOffset(xChRight - xChLeft, lineWidth, nextOffset);
            break;
          case 5 /* DASHED */:
            const lineRatio = 0.6;
            const gapRatio = 0.3;
            const xChWidth = xChRight - xChLeft;
            const line = Math.floor(lineRatio * xChWidth);
            const gap = Math.floor(gapRatio * xChWidth);
            const end = xChWidth - line - gap;
            this._tmpCtx.setLineDash([line, gap, end]);
            this._tmpCtx.moveTo(xChLeft, yTop);
            this._tmpCtx.lineTo(xChRight, yTop);
            break;
          case 1 /* SINGLE */:
          default:
            this._tmpCtx.moveTo(xChLeft, yTop);
            this._tmpCtx.lineTo(xChRight, yTop);
            break;
        }
        this._tmpCtx.stroke();
        this._tmpCtx.restore();
      }
      this._tmpCtx.restore();
      if (!customGlyph && this._config.fontSize >= 12) {
        if (!this._config.allowTransparency && chars !== " ") {
          this._tmpCtx.save();
          this._tmpCtx.textBaseline = "alphabetic";
          const metrics = this._tmpCtx.measureText(chars);
          this._tmpCtx.restore();
          if ("actualBoundingBoxDescent" in metrics && metrics.actualBoundingBoxDescent > 0) {
            this._tmpCtx.save();
            const clipRegion = new Path2D();
            clipRegion.rect(xLeft, yTop - Math.ceil(lineWidth / 2), this._config.deviceCellWidth * chWidth, yBot - yTop + Math.ceil(lineWidth / 2));
            this._tmpCtx.clip(clipRegion);
            this._tmpCtx.lineWidth = this._config.devicePixelRatio * 3;
            this._tmpCtx.strokeStyle = backgroundColor.css;
            this._tmpCtx.strokeText(chars, padding, padding + this._config.deviceCharHeight);
            this._tmpCtx.restore();
          }
        }
      }
    }
    if (overline) {
      const lineWidth = Math.max(1, Math.floor(this._config.fontSize * this._config.devicePixelRatio / 15));
      const yOffset = lineWidth % 2 === 1 ? 0.5 : 0;
      this._tmpCtx.lineWidth = lineWidth;
      this._tmpCtx.strokeStyle = this._tmpCtx.fillStyle;
      this._tmpCtx.beginPath();
      this._tmpCtx.moveTo(padding, padding + yOffset);
      this._tmpCtx.lineTo(padding + this._config.deviceCharWidth * chWidth, padding + yOffset);
      this._tmpCtx.stroke();
    }
    if (!customGlyph) {
      this._tmpCtx.fillText(chars, padding, padding + this._config.deviceCharHeight);
    }
    if (chars === "_" && !this._config.allowTransparency) {
      let isBeyondCellBounds = clearColor(this._tmpCtx.getImageData(padding, padding, this._config.deviceCellWidth, this._config.deviceCellHeight), backgroundColor, foregroundColor, enableClearThresholdCheck);
      if (isBeyondCellBounds) {
        for (let offset = 1; offset <= 5; offset++) {
          this._tmpCtx.save();
          this._tmpCtx.fillStyle = backgroundColor.css;
          this._tmpCtx.fillRect(0, 0, this._tmpCanvas.width, this._tmpCanvas.height);
          this._tmpCtx.restore();
          this._tmpCtx.fillText(chars, padding, padding + this._config.deviceCharHeight - offset);
          isBeyondCellBounds = clearColor(this._tmpCtx.getImageData(padding, padding, this._config.deviceCellWidth, this._config.deviceCellHeight), backgroundColor, foregroundColor, enableClearThresholdCheck);
          if (!isBeyondCellBounds) {
            break;
          }
        }
      }
    }
    if (strikethrough) {
      const lineWidth = Math.max(1, Math.floor(this._config.fontSize * this._config.devicePixelRatio / 10));
      const yOffset = this._tmpCtx.lineWidth % 2 === 1 ? 0.5 : 0;
      this._tmpCtx.lineWidth = lineWidth;
      this._tmpCtx.strokeStyle = this._tmpCtx.fillStyle;
      this._tmpCtx.beginPath();
      this._tmpCtx.moveTo(padding, padding + Math.floor(this._config.deviceCharHeight / 2) - yOffset);
      this._tmpCtx.lineTo(padding + this._config.deviceCharWidth * chWidth, padding + Math.floor(this._config.deviceCharHeight / 2) - yOffset);
      this._tmpCtx.stroke();
    }
    this._tmpCtx.restore();
    const imageData = this._tmpCtx.getImageData(
      0,
      0,
      this._tmpCanvas.width,
      this._tmpCanvas.height
    );
    let isEmpty;
    if (!this._config.allowTransparency) {
      isEmpty = clearColor(imageData, backgroundColor, foregroundColor, enableClearThresholdCheck);
    } else {
      isEmpty = checkCompletelyTransparent(imageData);
    }
    if (isEmpty) {
      return NULL_RASTERIZED_GLYPH;
    }
    const rasterizedGlyph = this._findGlyphBoundingBox(imageData, this._workBoundingBox, allowedWidth, restrictedPowerlineGlyph, customGlyph, padding);
    let activePage;
    let activeRow;
    while (true) {
      if (this._activePages.length === 0) {
        const newPage = this._createNewPage();
        activePage = newPage;
        activeRow = newPage.currentRow;
        activeRow.height = rasterizedGlyph.size.y;
        break;
      }
      activePage = this._activePages[this._activePages.length - 1];
      activeRow = activePage.currentRow;
      for (const p of this._activePages) {
        if (rasterizedGlyph.size.y <= p.currentRow.height) {
          activePage = p;
          activeRow = p.currentRow;
        }
      }
      for (let i = this._activePages.length - 1; i >= 0; i--) {
        for (const row of this._activePages[i].fixedRows) {
          if (row.height <= activeRow.height && rasterizedGlyph.size.y <= row.height) {
            activePage = this._activePages[i];
            activeRow = row;
          }
        }
      }
      if (rasterizedGlyph.size.x > this._textureSize) {
        if (!this._overflowSizePage) {
          if (_TextureAtlas.maxAtlasPages && this._pages.length >= _TextureAtlas.maxAtlasPages) {
            this._evictAllPages();
          }
          this._overflowSizePage = new AtlasPage(this._document, this._config.deviceMaxTextureSize);
          this.pages.push(this._overflowSizePage);
          this._pageLayoutVersion++;
          this._onAddTextureAtlasCanvas.fire(this._overflowSizePage.canvas);
        }
        activePage = this._overflowSizePage;
        activeRow = this._overflowSizePage.currentRow;
        if (activeRow.x + rasterizedGlyph.size.x >= activePage.canvas.width) {
          activeRow.x = 0;
          activeRow.y += activeRow.height;
          activeRow.height = 0;
        }
        break;
      }
      if (activeRow.y + rasterizedGlyph.size.y >= activePage.canvas.height || activeRow.height > rasterizedGlyph.size.y + 2 /* ROW_PIXEL_THRESHOLD */) {
        let wasPageAndRowFound = false;
        if (activePage.currentRow.y + activePage.currentRow.height + rasterizedGlyph.size.y >= activePage.canvas.height) {
          let candidatePage;
          for (const p of this._activePages) {
            if (p.currentRow.y + p.currentRow.height + rasterizedGlyph.size.y < p.canvas.height) {
              candidatePage = p;
              break;
            }
          }
          if (candidatePage) {
            activePage = candidatePage;
          } else {
            if (_TextureAtlas.maxAtlasPages && this._pages.length >= _TextureAtlas.maxAtlasPages && activeRow.y + rasterizedGlyph.size.y <= activePage.canvas.height && activeRow.height >= rasterizedGlyph.size.y && activeRow.x + rasterizedGlyph.size.x <= activePage.canvas.width) {
              wasPageAndRowFound = true;
            } else {
              const newPage = this._createNewPage();
              activePage = newPage;
              activeRow = newPage.currentRow;
              activeRow.height = rasterizedGlyph.size.y;
              wasPageAndRowFound = true;
            }
          }
        }
        if (!wasPageAndRowFound) {
          if (activePage.currentRow.height > 0) {
            activePage.fixedRows.push(activePage.currentRow);
          }
          activeRow = {
            x: 0,
            y: activePage.currentRow.y + activePage.currentRow.height,
            height: rasterizedGlyph.size.y
          };
          activePage.fixedRows.push(activeRow);
          activePage.currentRow = {
            x: 0,
            y: activeRow.y + activeRow.height,
            height: 0
          };
        }
      }
      if (activeRow.x + rasterizedGlyph.size.x <= activePage.canvas.width) {
        break;
      }
      if (activeRow === activePage.currentRow) {
        activeRow.x = 0;
        activeRow.y += activeRow.height;
        activeRow.height = 0;
      } else {
        activePage.fixedRows.splice(activePage.fixedRows.indexOf(activeRow), 1);
      }
    }
    rasterizedGlyph.texturePage = this._pages.indexOf(activePage);
    rasterizedGlyph.texturePosition.x = activeRow.x;
    rasterizedGlyph.texturePosition.y = activeRow.y;
    rasterizedGlyph.texturePositionClipSpace.x = activeRow.x / activePage.canvas.width;
    rasterizedGlyph.texturePositionClipSpace.y = activeRow.y / activePage.canvas.height;
    rasterizedGlyph.sizeClipSpace.x /= activePage.canvas.width;
    rasterizedGlyph.sizeClipSpace.y /= activePage.canvas.height;
    activeRow.height = Math.max(activeRow.height, rasterizedGlyph.size.y);
    activeRow.x += rasterizedGlyph.size.x;
    activePage.ctx.putImageData(
      imageData,
      rasterizedGlyph.texturePosition.x - this._workBoundingBox.left,
      rasterizedGlyph.texturePosition.y - this._workBoundingBox.top,
      this._workBoundingBox.left,
      this._workBoundingBox.top,
      rasterizedGlyph.size.x,
      rasterizedGlyph.size.y
    );
    activePage.addGlyph(rasterizedGlyph);
    activePage.version = ++AtlasPage.nextVersion;
    return rasterizedGlyph;
  }
  /**
   * Given an ImageData object, find the bounding box of the non-transparent
   * portion of the texture and return an IRasterizedGlyph with these
   * dimensions.
   * @param imageData The image data to read.
   * @param boundingBox An IBoundingBox to put the clipped bounding box values.
   */
  _findGlyphBoundingBox(imageData, boundingBox, allowedWidth, restrictedGlyph, customGlyph, padding) {
    boundingBox.top = 0;
    const height = restrictedGlyph ? this._config.deviceCellHeight : this._tmpCanvas.height;
    const width = restrictedGlyph ? this._config.deviceCellWidth : allowedWidth;
    let found = false;
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        const alphaOffset = y * this._tmpCanvas.width * 4 + x * 4 + 3;
        if (imageData.data[alphaOffset] !== 0) {
          boundingBox.top = y;
          found = true;
          break;
        }
      }
      if (found) {
        break;
      }
    }
    boundingBox.left = 0;
    found = false;
    for (let x = 0; x < padding + width; x++) {
      for (let y = 0; y < height; y++) {
        const alphaOffset = y * this._tmpCanvas.width * 4 + x * 4 + 3;
        if (imageData.data[alphaOffset] !== 0) {
          boundingBox.left = x;
          found = true;
          break;
        }
      }
      if (found) {
        break;
      }
    }
    boundingBox.right = width;
    found = false;
    for (let x = padding + width - 1; x >= padding; x--) {
      for (let y = 0; y < height; y++) {
        const alphaOffset = y * this._tmpCanvas.width * 4 + x * 4 + 3;
        if (imageData.data[alphaOffset] !== 0) {
          boundingBox.right = x;
          found = true;
          break;
        }
      }
      if (found) {
        break;
      }
    }
    boundingBox.bottom = height;
    found = false;
    for (let y = height - 1; y >= 0; y--) {
      for (let x = 0; x < width; x++) {
        const alphaOffset = y * this._tmpCanvas.width * 4 + x * 4 + 3;
        if (imageData.data[alphaOffset] !== 0) {
          boundingBox.bottom = y;
          found = true;
          break;
        }
      }
      if (found) {
        break;
      }
    }
    return {
      texturePage: 0,
      texturePosition: { x: 0, y: 0 },
      texturePositionClipSpace: { x: 0, y: 0 },
      size: {
        x: boundingBox.right - boundingBox.left + 1,
        y: boundingBox.bottom - boundingBox.top + 1
      },
      sizeClipSpace: {
        x: boundingBox.right - boundingBox.left + 1,
        y: boundingBox.bottom - boundingBox.top + 1
      },
      offset: {
        x: -boundingBox.left + padding + (restrictedGlyph || customGlyph ? Math.floor((this._config.deviceCellWidth - this._config.deviceCharWidth) / 2) : 0),
        y: -boundingBox.top + padding + (restrictedGlyph || customGlyph ? this._config.lineHeight === 1 ? 0 : Math.round((this._config.deviceCellHeight - this._config.deviceCharHeight) / 2) : 0)
      }
    };
  }
};
var _AtlasPage = class _AtlasPage {
  constructor(document2, size, sourcePages) {
    this._usedPixels = 0;
    this._glyphs = [];
    this.version = ++_AtlasPage.nextVersion;
    // Texture atlas current positioning data. The texture packing strategy used is to fill from
    // left-to-right and top-to-bottom. When the glyph being written is less than half of the current
    // row's height, the following happens:
    //
    // - The current row becomes the fixed height row A
    // - A new fixed height row B the exact size of the glyph is created below the current row
    // - A new dynamic height current row is created below B
    //
    // This strategy does a good job preventing space being wasted for very short glyphs such as
    // underscores, hyphens etc. or those with underlines rendered.
    this.currentRow = {
      x: 0,
      y: 0,
      height: 0
    };
    this.fixedRows = [];
    if (sourcePages) {
      for (const p of sourcePages) {
        this._glyphs.push(...p.glyphs);
        this._usedPixels += p._usedPixels;
      }
    }
    this.canvas = createCanvas(document2, size, size);
    this.ctx = throwIfFalsy(this.canvas.getContext("2d", { alpha: true }));
  }
  get percentageUsed() {
    return this._usedPixels / (this.canvas.width * this.canvas.height);
  }
  get glyphs() {
    return this._glyphs;
  }
  addGlyph(glyph) {
    this._glyphs.push(glyph);
    this._usedPixels += glyph.size.x * glyph.size.y;
  }
  clear() {
    this.ctx.clearRect(0, 0, this.canvas.width, this.canvas.height);
    this.currentRow.x = 0;
    this.currentRow.y = 0;
    this.currentRow.height = 0;
    this.fixedRows.length = 0;
    this.version = ++_AtlasPage.nextVersion;
  }
};
/**
 * Monotonically increasing across all atlas pages globally. Used to detect when the texture
 * unit at a given index needs to be re-uploaded — both for content changes within the same
 * page and for a page object swap at the same index (which happens after a page merge, where a
 * per-page counter could coincide with the previously-bound page's value; xterm.js #5883).
 */
_AtlasPage.nextVersion = 0;
var AtlasPage = _AtlasPage;
function clearColor(imageData, bg, fg, enableThresholdCheck) {
  const r = bg.rgba >>> 24;
  const g = bg.rgba >>> 16 & 255;
  const b = bg.rgba >>> 8 & 255;
  const fgR = fg.rgba >>> 24;
  const fgG = fg.rgba >>> 16 & 255;
  const fgB = fg.rgba >>> 8 & 255;
  const threshold = Math.floor((Math.abs(r - fgR) + Math.abs(g - fgG) + Math.abs(b - fgB)) / 12);
  let isEmpty = true;
  for (let offset = 0; offset < imageData.data.length; offset += 4) {
    if (imageData.data[offset] === r && imageData.data[offset + 1] === g && imageData.data[offset + 2] === b) {
      imageData.data[offset + 3] = 0;
    } else {
      if (enableThresholdCheck && Math.abs(imageData.data[offset] - r) + Math.abs(imageData.data[offset + 1] - g) + Math.abs(imageData.data[offset + 2] - b) < threshold) {
        imageData.data[offset + 3] = 0;
      } else {
        isEmpty = false;
      }
    }
  }
  return isEmpty;
}
function checkCompletelyTransparent(imageData) {
  for (let offset = 0; offset < imageData.data.length; offset += 4) {
    if (imageData.data[offset + 3] > 0) {
      return false;
    }
  }
  return true;
}
function createCanvas(document2, width, height) {
  const canvas = document2.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  return canvas;
}

// vendor/xterm-addon-webgl/src/CharAtlasUtils.ts
function generateConfig(deviceCellWidth, deviceCellHeight, deviceCharWidth, deviceCharHeight, options, colors, devicePixelRatio, deviceMaxTextureSize) {
  const clonedColors = {
    foreground: colors.foreground,
    background: colors.background,
    cursor: NULL_COLOR,
    cursorAccent: NULL_COLOR,
    selectionForeground: NULL_COLOR,
    selectionBackgroundTransparent: NULL_COLOR,
    selectionBackgroundOpaque: NULL_COLOR,
    selectionInactiveBackgroundTransparent: NULL_COLOR,
    selectionInactiveBackgroundOpaque: NULL_COLOR,
    overviewRulerBorder: NULL_COLOR,
    scrollbarSliderBackground: NULL_COLOR,
    scrollbarSliderHoverBackground: NULL_COLOR,
    scrollbarSliderActiveBackground: NULL_COLOR,
    // For the static char atlas, we only use the first 16 colors, but we need all 256 for the
    // dynamic character atlas.
    ansi: colors.ansi.slice(),
    contrastCache: colors.contrastCache,
    halfContrastCache: colors.halfContrastCache
  };
  return {
    customGlyphs: options.customGlyphs,
    devicePixelRatio,
    deviceMaxTextureSize,
    letterSpacing: options.letterSpacing,
    lineHeight: options.lineHeight,
    deviceCellWidth,
    deviceCellHeight,
    deviceCharWidth,
    deviceCharHeight,
    fontFamily: options.fontFamily,
    fontSize: options.fontSize,
    fontWeight: options.fontWeight,
    fontWeightBold: options.fontWeightBold,
    allowTransparency: options.allowTransparency,
    drawBoldTextInBrightColors: options.drawBoldTextInBrightColors,
    minimumContrastRatio: options.minimumContrastRatio,
    colors: clonedColors
  };
}
function configEquals(a, b) {
  for (let i = 0; i < a.colors.ansi.length; i++) {
    if (a.colors.ansi[i].rgba !== b.colors.ansi[i].rgba) {
      return false;
    }
  }
  return a.devicePixelRatio === b.devicePixelRatio && a.deviceMaxTextureSize === b.deviceMaxTextureSize && a.deviceCellWidth === b.deviceCellWidth && a.deviceCellHeight === b.deviceCellHeight && a.customGlyphs === b.customGlyphs && a.lineHeight === b.lineHeight && a.letterSpacing === b.letterSpacing && a.fontFamily === b.fontFamily && a.fontSize === b.fontSize && a.fontWeight === b.fontWeight && a.fontWeightBold === b.fontWeightBold && a.allowTransparency === b.allowTransparency && a.deviceCharWidth === b.deviceCharWidth && a.deviceCharHeight === b.deviceCharHeight && a.drawBoldTextInBrightColors === b.drawBoldTextInBrightColors && a.minimumContrastRatio === b.minimumContrastRatio && a.colors.foreground.rgba === b.colors.foreground.rgba && a.colors.background.rgba === b.colors.background.rgba;
}
function is256Color(colorCode) {
  return (colorCode & 50331648 /* CM_MASK */) === 16777216 /* CM_P16 */ || (colorCode & 50331648 /* CM_MASK */) === 33554432 /* CM_P256 */;
}

// vendor/xterm-addon-webgl/src/CharAtlasCache.ts
var charAtlasCache = [];
function acquireTextureAtlas(terminal, options, colors, deviceCellWidth, deviceCellHeight, deviceCharWidth, deviceCharHeight, devicePixelRatio, deviceMaxTextureSize) {
  const newConfig = generateConfig(deviceCellWidth, deviceCellHeight, deviceCharWidth, deviceCharHeight, options, colors, devicePixelRatio, deviceMaxTextureSize);
  for (let i = 0; i < charAtlasCache.length; i++) {
    const entry = charAtlasCache[i];
    const ownedByIndex = entry.ownedBy.indexOf(terminal);
    if (ownedByIndex >= 0) {
      if (configEquals(entry.config, newConfig)) {
        return entry.atlas;
      }
      if (entry.ownedBy.length === 1) {
        entry.atlas.dispose();
        charAtlasCache.splice(i, 1);
      } else {
        entry.ownedBy.splice(ownedByIndex, 1);
      }
      break;
    }
  }
  for (let i = 0; i < charAtlasCache.length; i++) {
    const entry = charAtlasCache[i];
    if (configEquals(entry.config, newConfig)) {
      entry.ownedBy.push(terminal);
      return entry.atlas;
    }
  }
  const core = terminal._core;
  const newEntry = {
    atlas: new TextureAtlas(document, newConfig, core.unicodeService),
    config: newConfig,
    ownedBy: [terminal]
  };
  charAtlasCache.push(newEntry);
  return newEntry.atlas;
}
function removeTerminalFromCache(terminal) {
  for (let i = 0; i < charAtlasCache.length; i++) {
    const index = charAtlasCache[i].ownedBy.indexOf(terminal);
    if (index !== -1) {
      if (charAtlasCache[i].ownedBy.length === 1) {
        charAtlasCache[i].atlas.dispose();
        charAtlasCache.splice(i, 1);
      } else {
        charAtlasCache[i].ownedBy.splice(index, 1);
      }
      break;
    }
  }
}

// vendor/xterm-addon-webgl/src/CursorBlinkStateManager.ts
var BLINK_INTERVAL = 600;
var CursorBlinkStateManager = class {
  constructor(_renderCallback, _coreBrowserService) {
    this._renderCallback = _renderCallback;
    this._coreBrowserService = _coreBrowserService;
    this.isCursorVisible = true;
    if (this._coreBrowserService.isFocused) {
      this._restartInterval();
    }
  }
  get isPaused() {
    return !(this._blinkStartTimeout || this._blinkInterval);
  }
  dispose() {
    if (this._blinkInterval) {
      this._coreBrowserService.window.clearInterval(this._blinkInterval);
      this._blinkInterval = void 0;
    }
    if (this._blinkStartTimeout) {
      this._coreBrowserService.window.clearTimeout(this._blinkStartTimeout);
      this._blinkStartTimeout = void 0;
    }
    if (this._animationFrame) {
      this._coreBrowserService.window.cancelAnimationFrame(this._animationFrame);
      this._animationFrame = void 0;
    }
  }
  restartBlinkAnimation() {
    if (this.isPaused) {
      return;
    }
    this._animationTimeRestarted = Date.now();
    this.isCursorVisible = true;
    if (!this._animationFrame) {
      this._animationFrame = this._coreBrowserService.window.requestAnimationFrame(() => {
        this._renderCallback();
        this._animationFrame = void 0;
      });
    }
  }
  _restartInterval(timeToStart = BLINK_INTERVAL) {
    if (this._blinkInterval) {
      this._coreBrowserService.window.clearInterval(this._blinkInterval);
      this._blinkInterval = void 0;
    }
    this._blinkStartTimeout = this._coreBrowserService.window.setTimeout(() => {
      if (this._animationTimeRestarted) {
        const time = BLINK_INTERVAL - (Date.now() - this._animationTimeRestarted);
        this._animationTimeRestarted = void 0;
        if (time > 0) {
          this._restartInterval(time);
          return;
        }
      }
      this.isCursorVisible = false;
      this._animationFrame = this._coreBrowserService.window.requestAnimationFrame(() => {
        this._renderCallback();
        this._animationFrame = void 0;
      });
      this._blinkInterval = this._coreBrowserService.window.setInterval(() => {
        if (this._animationTimeRestarted) {
          const time = BLINK_INTERVAL - (Date.now() - this._animationTimeRestarted);
          this._animationTimeRestarted = void 0;
          this._restartInterval(time);
          return;
        }
        this.isCursorVisible = !this.isCursorVisible;
        this._animationFrame = this._coreBrowserService.window.requestAnimationFrame(() => {
          this._renderCallback();
          this._animationFrame = void 0;
        });
      }, BLINK_INTERVAL);
    }, timeToStart);
  }
  pause() {
    this.isCursorVisible = true;
    if (this._blinkInterval) {
      this._coreBrowserService.window.clearInterval(this._blinkInterval);
      this._blinkInterval = void 0;
    }
    if (this._blinkStartTimeout) {
      this._coreBrowserService.window.clearTimeout(this._blinkStartTimeout);
      this._blinkStartTimeout = void 0;
    }
    if (this._animationFrame) {
      this._coreBrowserService.window.cancelAnimationFrame(this._animationFrame);
      this._animationFrame = void 0;
    }
  }
  resume() {
    this.pause();
    this._animationTimeRestarted = void 0;
    this._restartInterval();
    this.restartBlinkAnimation();
  }
};

// vendor/xterm-addon-webgl/src/DevicePixelObserver.ts
function observeDevicePixelDimensions(element, parentWindow, callback) {
  let observer = new parentWindow.ResizeObserver((entries) => {
    const entry = entries.find((entry2) => entry2.target === element);
    if (!entry) {
      return;
    }
    if (!("devicePixelContentBoxSize" in entry)) {
      observer?.disconnect();
      observer = void 0;
      return;
    }
    const width = entry.devicePixelContentBoxSize[0].inlineSize;
    const height = entry.devicePixelContentBoxSize[0].blockSize;
    if (width > 0 && height > 0) {
      callback(width, height);
    }
  });
  try {
    observer.observe(element, { box: ["device-pixel-content-box"] });
  } catch {
    observer.disconnect();
    observer = void 0;
  }
  return toDisposable(() => observer?.disconnect());
}

// node_modules/@xterm/xterm/src/common/input/TextDecoder.ts
function stringFromCodePoint(codePoint) {
  if (codePoint > 65535) {
    codePoint -= 65536;
    return String.fromCharCode((codePoint >> 10) + 55296) + String.fromCharCode(codePoint % 1024 + 56320);
  }
  return String.fromCharCode(codePoint);
}

// node_modules/@xterm/xterm/src/common/buffer/CellData.ts
var CellData = class _CellData extends AttributeData {
  constructor() {
    super(...arguments);
    /** Primitives from terminal buffer. */
    this.content = 0;
    this.fg = 0;
    this.bg = 0;
    this.extended = new ExtendedAttrs();
    this.combinedData = "";
  }
  /** Helper to create CellData from CharData. */
  static fromCharData(value) {
    const obj = new _CellData();
    obj.setFromCharData(value);
    return obj;
  }
  /** Whether cell contains a combined string. */
  isCombined() {
    return this.content & 2097152 /* IS_COMBINED_MASK */;
  }
  /** Width of the cell. */
  getWidth() {
    return this.content >> 22 /* WIDTH_SHIFT */;
  }
  /** JS string of the content. */
  getChars() {
    if (this.content & 2097152 /* IS_COMBINED_MASK */) {
      return this.combinedData;
    }
    if (this.content & 2097151 /* CODEPOINT_MASK */) {
      return stringFromCodePoint(this.content & 2097151 /* CODEPOINT_MASK */);
    }
    return "";
  }
  /**
   * Codepoint of cell
   * Note this returns the UTF32 codepoint of single chars,
   * if content is a combined string it returns the codepoint
   * of the last char in string to be in line with code in CharData.
   */
  getCode() {
    return this.isCombined() ? this.combinedData.charCodeAt(this.combinedData.length - 1) : this.content & 2097151 /* CODEPOINT_MASK */;
  }
  /** Set data from CharData */
  setFromCharData(value) {
    this.fg = value[CHAR_DATA_ATTR_INDEX];
    this.bg = 0;
    let combined = false;
    if (value[CHAR_DATA_CHAR_INDEX].length > 2) {
      combined = true;
    } else if (value[CHAR_DATA_CHAR_INDEX].length === 2) {
      const code = value[CHAR_DATA_CHAR_INDEX].charCodeAt(0);
      if (55296 <= code && code <= 56319) {
        const second = value[CHAR_DATA_CHAR_INDEX].charCodeAt(1);
        if (56320 <= second && second <= 57343) {
          this.content = (code - 55296) * 1024 + second - 56320 + 65536 | value[CHAR_DATA_WIDTH_INDEX] << 22 /* WIDTH_SHIFT */;
        } else {
          combined = true;
        }
      } else {
        combined = true;
      }
    } else {
      this.content = value[CHAR_DATA_CHAR_INDEX].charCodeAt(0) | value[CHAR_DATA_WIDTH_INDEX] << 22 /* WIDTH_SHIFT */;
    }
    if (combined) {
      this.combinedData = value[CHAR_DATA_CHAR_INDEX];
      this.content = 2097152 /* IS_COMBINED_MASK */ | value[CHAR_DATA_WIDTH_INDEX] << 22 /* WIDTH_SHIFT */;
    }
  }
  /** Get data as CharData. */
  getAsCharData() {
    return [this.fg, this.getChars(), this.getWidth(), this.getCode()];
  }
};

// vendor/xterm-addon-webgl/src/WebglUtils.ts
var PROJECTION_MATRIX = new Float32Array([
  2,
  0,
  0,
  0,
  0,
  -2,
  0,
  0,
  0,
  0,
  1,
  0,
  -1,
  1,
  0,
  1
]);
function createProgram(gl, vertexSource, fragmentSource) {
  const program = throwIfFalsy(gl.createProgram());
  gl.attachShader(program, throwIfFalsy(createShader(gl, gl.VERTEX_SHADER, vertexSource)));
  gl.attachShader(program, throwIfFalsy(createShader(gl, gl.FRAGMENT_SHADER, fragmentSource)));
  gl.linkProgram(program);
  const success = gl.getProgramParameter(program, gl.LINK_STATUS);
  if (success) {
    return program;
  }
  console.error(gl.getProgramInfoLog(program));
  gl.deleteProgram(program);
}
function createShader(gl, type, source) {
  const shader = throwIfFalsy(gl.createShader(type));
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  const success = gl.getShaderParameter(shader, gl.COMPILE_STATUS);
  if (success) {
    return shader;
  }
  console.error(gl.getShaderInfoLog(shader));
  gl.deleteShader(shader);
}
function expandFloat32Array(source, max) {
  const newLength = Math.min(source.length * 2, max);
  const newArray = new Float32Array(newLength);
  for (let i = 0; i < source.length; i++) {
    newArray[i] = source[i];
  }
  return newArray;
}
var GLTexture = class {
  constructor(texture) {
    this.texture = texture;
    this.version = -1;
  }
};

// vendor/xterm-addon-webgl/src/GlyphRenderer.ts
var vertexShaderSource = `#version 300 es
layout (location = ${0 /* UNIT_QUAD */}) in vec2 a_unitquad;
layout (location = ${1 /* CELL_POSITION */}) in vec2 a_cellpos;
layout (location = ${2 /* OFFSET */}) in vec2 a_offset;
layout (location = ${3 /* SIZE */}) in vec2 a_size;
layout (location = ${4 /* TEXPAGE */}) in float a_texpage;
layout (location = ${5 /* TEXCOORD */}) in vec2 a_texcoord;
layout (location = ${6 /* TEXSIZE */}) in vec2 a_texsize;

uniform mat4 u_projection;
uniform vec2 u_resolution;

out vec2 v_texcoord;
flat out int v_texpage;

void main() {
  vec2 zeroToOne = (a_offset / u_resolution) + a_cellpos + (a_unitquad * a_size);
  gl_Position = u_projection * vec4(zeroToOne, 0.0, 1.0);
  v_texpage = int(a_texpage);
  v_texcoord = a_texcoord + a_unitquad * a_texsize;
}`;
function createFragmentShaderSource(maxFragmentShaderTextureUnits) {
  let textureConditionals = "";
  for (let i = 1; i < maxFragmentShaderTextureUnits; i++) {
    textureConditionals += ` else if (v_texpage == ${i}) { outColor = texture(u_texture[${i}], v_texcoord); }`;
  }
  return `#version 300 es
precision lowp float;

in vec2 v_texcoord;
flat in int v_texpage;

uniform sampler2D u_texture[${maxFragmentShaderTextureUnits}];

out vec4 outColor;

void main() {
  if (v_texpage == 0) {
    outColor = texture(u_texture[0], v_texcoord);
  } ${textureConditionals}
}`;
}
var INDICES_PER_CELL = 11;
var BYTES_PER_CELL = INDICES_PER_CELL * Float32Array.BYTES_PER_ELEMENT;
var CELL_POSITION_INDICES = 2;
var $i = 0;
var $glyph2 = void 0;
var $leftCellPadding = 0;
var $clippedPixels = 0;
var GlyphRenderer = class extends Disposable {
  constructor(_terminal, _gl, _dimensions, _optionsService) {
    super();
    this._terminal = _terminal;
    this._gl = _gl;
    this._dimensions = _dimensions;
    this._optionsService = _optionsService;
    this._lastSeenPageLayoutVersion = -1;
    this._pageOverflowWarned = false;
    this._activeBuffer = 0;
    this._vertices = {
      count: 0,
      attributes: new Float32Array(0),
      attributesBuffers: [
        new Float32Array(0),
        new Float32Array(0)
      ]
    };
    const gl = this._gl;
    if (TextureAtlas.maxAtlasPages === void 0) {
      TextureAtlas.maxAtlasPages = Math.min(32, throwIfFalsy(gl.getParameter(gl.MAX_TEXTURE_IMAGE_UNITS)));
      TextureAtlas.maxTextureSize = throwIfFalsy(gl.getParameter(gl.MAX_TEXTURE_SIZE));
    }
    this._program = throwIfFalsy(createProgram(gl, vertexShaderSource, createFragmentShaderSource(TextureAtlas.maxAtlasPages)));
    this._register(toDisposable(() => gl.deleteProgram(this._program)));
    this._projectionLocation = throwIfFalsy(gl.getUniformLocation(this._program, "u_projection"));
    this._resolutionLocation = throwIfFalsy(gl.getUniformLocation(this._program, "u_resolution"));
    this._textureLocation = throwIfFalsy(gl.getUniformLocation(this._program, "u_texture"));
    this._vertexArrayObject = gl.createVertexArray();
    gl.bindVertexArray(this._vertexArrayObject);
    const unitQuadVertices = new Float32Array([0, 0, 1, 0, 0, 1, 1, 1]);
    const unitQuadVerticesBuffer = gl.createBuffer();
    this._register(toDisposable(() => gl.deleteBuffer(unitQuadVerticesBuffer)));
    gl.bindBuffer(gl.ARRAY_BUFFER, unitQuadVerticesBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, unitQuadVertices, gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0 /* UNIT_QUAD */);
    gl.vertexAttribPointer(0 /* UNIT_QUAD */, 2, this._gl.FLOAT, false, 0, 0);
    const unitQuadElementIndices = new Uint8Array([0, 1, 2, 3]);
    const elementIndicesBuffer = gl.createBuffer();
    this._register(toDisposable(() => gl.deleteBuffer(elementIndicesBuffer)));
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, elementIndicesBuffer);
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, unitQuadElementIndices, gl.STATIC_DRAW);
    this._attributesBuffer = throwIfFalsy(gl.createBuffer());
    this._register(toDisposable(() => gl.deleteBuffer(this._attributesBuffer)));
    gl.bindBuffer(gl.ARRAY_BUFFER, this._attributesBuffer);
    gl.enableVertexAttribArray(2 /* OFFSET */);
    gl.vertexAttribPointer(2 /* OFFSET */, 2, gl.FLOAT, false, BYTES_PER_CELL, 0);
    gl.vertexAttribDivisor(2 /* OFFSET */, 1);
    gl.enableVertexAttribArray(3 /* SIZE */);
    gl.vertexAttribPointer(3 /* SIZE */, 2, gl.FLOAT, false, BYTES_PER_CELL, 2 * Float32Array.BYTES_PER_ELEMENT);
    gl.vertexAttribDivisor(3 /* SIZE */, 1);
    gl.enableVertexAttribArray(4 /* TEXPAGE */);
    gl.vertexAttribPointer(4 /* TEXPAGE */, 1, gl.FLOAT, false, BYTES_PER_CELL, 4 * Float32Array.BYTES_PER_ELEMENT);
    gl.vertexAttribDivisor(4 /* TEXPAGE */, 1);
    gl.enableVertexAttribArray(5 /* TEXCOORD */);
    gl.vertexAttribPointer(5 /* TEXCOORD */, 2, gl.FLOAT, false, BYTES_PER_CELL, 5 * Float32Array.BYTES_PER_ELEMENT);
    gl.vertexAttribDivisor(5 /* TEXCOORD */, 1);
    gl.enableVertexAttribArray(6 /* TEXSIZE */);
    gl.vertexAttribPointer(6 /* TEXSIZE */, 2, gl.FLOAT, false, BYTES_PER_CELL, 7 * Float32Array.BYTES_PER_ELEMENT);
    gl.vertexAttribDivisor(6 /* TEXSIZE */, 1);
    gl.enableVertexAttribArray(1 /* CELL_POSITION */);
    gl.vertexAttribPointer(1 /* CELL_POSITION */, 2, gl.FLOAT, false, BYTES_PER_CELL, 9 * Float32Array.BYTES_PER_ELEMENT);
    gl.vertexAttribDivisor(1 /* CELL_POSITION */, 1);
    gl.useProgram(this._program);
    const textureUnits = new Int32Array(TextureAtlas.maxAtlasPages);
    for (let i = 0; i < TextureAtlas.maxAtlasPages; i++) {
      textureUnits[i] = i;
    }
    gl.uniform1iv(this._textureLocation, textureUnits);
    gl.uniformMatrix4fv(this._projectionLocation, false, PROJECTION_MATRIX);
    this._atlasTextures = [];
    for (let i = 0; i < TextureAtlas.maxAtlasPages; i++) {
      const glTexture = new GLTexture(throwIfFalsy(gl.createTexture()));
      this._register(toDisposable(() => gl.deleteTexture(glTexture.texture)));
      gl.activeTexture(gl.TEXTURE0 + i);
      gl.bindTexture(gl.TEXTURE_2D, glTexture.texture);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 1, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array([255, 0, 0, 255]));
      this._atlasTextures[i] = glTexture;
    }
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
    this.handleResize();
  }
  /**
   * Call when a frame is being drawn. Returns whether the full model must be rebuilt before
   * rendering this frame because the atlas page layout changed since this renderer last drew
   * (tracked per renderer — the atlas is shared between terminals).
   */
  beginFrame() {
    if (!this._atlas) {
      return true;
    }
    if (this._atlas.pageLayoutVersion !== this._lastSeenPageLayoutVersion) {
      this._lastSeenPageLayoutVersion = this._atlas.pageLayoutVersion;
      return true;
    }
    return false;
  }
  updateCell(x, y, code, bg, fg, ext, chars, width, lastBg) {
    this._updateCell(this._vertices.attributes, x, y, code, bg, fg, ext, chars, width, lastBg);
  }
  _updateCell(array, x, y, code, bg, fg, ext, chars, width, lastBg) {
    $i = (y * this._terminal.cols + x) * INDICES_PER_CELL;
    if (code === NULL_CELL_CODE || code === void 0) {
      array.fill(0, $i, $i + INDICES_PER_CELL - 1 - CELL_POSITION_INDICES);
      return;
    }
    if (!this._atlas) {
      return;
    }
    if (chars && chars.length > 1) {
      $glyph2 = this._atlas.getRasterizedGlyphCombinedChar(chars, bg, fg, ext, false, this._terminal.element);
    } else {
      $glyph2 = this._atlas.getRasterizedGlyph(code, bg, fg, ext, false, this._terminal.element);
    }
    $leftCellPadding = Math.floor((this._dimensions.device.cell.width - this._dimensions.device.char.width) / 2);
    if (bg !== lastBg && $glyph2.offset.x > $leftCellPadding) {
      $clippedPixels = $glyph2.offset.x - $leftCellPadding;
      array[$i] = -($glyph2.offset.x - $clippedPixels) + this._dimensions.device.char.left;
      array[$i + 1] = -$glyph2.offset.y + this._dimensions.device.char.top;
      array[$i + 2] = ($glyph2.size.x - $clippedPixels) / this._dimensions.device.canvas.width;
      array[$i + 3] = $glyph2.size.y / this._dimensions.device.canvas.height;
      array[$i + 4] = $glyph2.texturePage;
      array[$i + 5] = $glyph2.texturePositionClipSpace.x + $clippedPixels / this._atlas.pages[$glyph2.texturePage].canvas.width;
      array[$i + 6] = $glyph2.texturePositionClipSpace.y;
      array[$i + 7] = $glyph2.sizeClipSpace.x - $clippedPixels / this._atlas.pages[$glyph2.texturePage].canvas.width;
      array[$i + 8] = $glyph2.sizeClipSpace.y;
    } else {
      array[$i] = -$glyph2.offset.x + this._dimensions.device.char.left;
      array[$i + 1] = -$glyph2.offset.y + this._dimensions.device.char.top;
      array[$i + 2] = $glyph2.size.x / this._dimensions.device.canvas.width;
      array[$i + 3] = $glyph2.size.y / this._dimensions.device.canvas.height;
      array[$i + 4] = $glyph2.texturePage;
      array[$i + 5] = $glyph2.texturePositionClipSpace.x;
      array[$i + 6] = $glyph2.texturePositionClipSpace.y;
      array[$i + 7] = $glyph2.sizeClipSpace.x;
      array[$i + 8] = $glyph2.sizeClipSpace.y;
    }
    if (this._optionsService.rawOptions.rescaleOverlappingGlyphs) {
      if (allowRescaling(code, width, $glyph2.size.x, this._dimensions.device.cell.width)) {
        array[$i + 2] = (this._dimensions.device.cell.width - 1) / this._dimensions.device.canvas.width;
      }
    }
  }
  clear() {
    const terminal = this._terminal;
    const newCount = terminal.cols * terminal.rows * INDICES_PER_CELL;
    if (this._vertices.count !== newCount) {
      this._vertices.attributes = new Float32Array(newCount);
    } else {
      this._vertices.attributes.fill(0);
    }
    let i = 0;
    for (; i < this._vertices.attributesBuffers.length; i++) {
      if (this._vertices.count !== newCount) {
        this._vertices.attributesBuffers[i] = new Float32Array(newCount);
      } else {
        this._vertices.attributesBuffers[i].fill(0);
      }
    }
    this._vertices.count = newCount;
    i = 0;
    for (let y = 0; y < terminal.rows; y++) {
      for (let x = 0; x < terminal.cols; x++) {
        this._vertices.attributes[i + 9] = x / terminal.cols;
        this._vertices.attributes[i + 10] = y / terminal.rows;
        i += INDICES_PER_CELL;
      }
    }
  }
  handleResize() {
    const gl = this._gl;
    gl.useProgram(this._program);
    gl.viewport(0, 0, gl.canvas.width, gl.canvas.height);
    gl.uniform2f(this._resolutionLocation, gl.canvas.width, gl.canvas.height);
    this.clear();
  }
  render(renderModel) {
    if (!this._atlas) {
      return;
    }
    const gl = this._gl;
    gl.useProgram(this._program);
    gl.bindVertexArray(this._vertexArrayObject);
    this._activeBuffer = (this._activeBuffer + 1) % 2;
    const activeBuffer = this._vertices.attributesBuffers[this._activeBuffer];
    let bufferLength = 0;
    for (let y = 0; y < renderModel.lineLengths.length; y++) {
      const si = y * this._terminal.cols * INDICES_PER_CELL;
      const sub = this._vertices.attributes.subarray(si, si + renderModel.lineLengths[y] * INDICES_PER_CELL);
      activeBuffer.set(sub, bufferLength);
      bufferLength += sub.length;
    }
    gl.bindBuffer(gl.ARRAY_BUFFER, this._attributesBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, activeBuffer.subarray(0, bufferLength), gl.STREAM_DRAW);
    const pageCount = Math.min(this._atlas.pages.length, this._atlasTextures.length);
    if (this._atlas.pages.length > this._atlasTextures.length && !this._pageOverflowWarned) {
      this._pageOverflowWarned = true;
      console.warn(`xterm webgl: atlas page count (${this._atlas.pages.length}) exceeds the renderer's texture capacity (${this._atlasTextures.length}), some glyphs will not render correctly`);
    }
    for (let i = 0; i < pageCount; i++) {
      if (this._atlas.pages[i].version !== this._atlasTextures[i].version) {
        this._bindAtlasPageTexture(gl, this._atlas, i);
      }
    }
    gl.drawElementsInstanced(gl.TRIANGLE_STRIP, 4, gl.UNSIGNED_BYTE, 0, bufferLength / INDICES_PER_CELL);
  }
  setAtlas(atlas) {
    this._atlas = atlas;
    this._lastSeenPageLayoutVersion = -1;
    this.invalidateAtlasTextures();
  }
  /** Forces every atlas page texture to be re-uploaded on the next render. */
  invalidateAtlasTextures() {
    for (const glTexture of this._atlasTextures) {
      glTexture.version = -1;
    }
  }
  _bindAtlasPageTexture(gl, atlas, i) {
    gl.activeTexture(gl.TEXTURE0 + i);
    gl.bindTexture(gl.TEXTURE_2D, this._atlasTextures[i].texture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, atlas.pages[i].canvas);
    this._atlasTextures[i].version = atlas.pages[i].version;
  }
  setDimensions(dimensions) {
    this._dimensions = dimensions;
  }
};

// node_modules/@xterm/xterm/src/browser/renderer/shared/SelectionRenderModel.ts
var SelectionRenderModel = class {
  constructor() {
    this.clear();
  }
  clear() {
    this.hasSelection = false;
    this.columnSelectMode = false;
    this.viewportStartRow = 0;
    this.viewportEndRow = 0;
    this.viewportCappedStartRow = 0;
    this.viewportCappedEndRow = 0;
    this.startCol = 0;
    this.endCol = 0;
    this.selectionStart = void 0;
    this.selectionEnd = void 0;
  }
  update(terminal, start, end, columnSelectMode = false) {
    this.selectionStart = start;
    this.selectionEnd = end;
    if (!start || !end || start[0] === end[0] && start[1] === end[1]) {
      this.clear();
      return;
    }
    const viewportY = terminal.buffers.active.ydisp;
    const viewportStartRow = start[1] - viewportY;
    const viewportEndRow = end[1] - viewportY;
    const viewportCappedStartRow = Math.max(viewportStartRow, 0);
    const viewportCappedEndRow = Math.min(viewportEndRow, terminal.rows - 1);
    if (viewportCappedStartRow >= terminal.rows || viewportCappedEndRow < 0) {
      this.clear();
      return;
    }
    this.hasSelection = true;
    this.columnSelectMode = columnSelectMode;
    this.viewportStartRow = viewportStartRow;
    this.viewportEndRow = viewportEndRow;
    this.viewportCappedStartRow = viewportCappedStartRow;
    this.viewportCappedEndRow = viewportCappedEndRow;
    this.startCol = start[0];
    this.endCol = end[0];
  }
  isCellSelected(terminal, x, y) {
    if (!this.hasSelection) {
      return false;
    }
    y -= terminal.buffer.active.viewportY;
    if (this.columnSelectMode) {
      if (this.startCol <= this.endCol) {
        return x >= this.startCol && y >= this.viewportCappedStartRow && x < this.endCol && y <= this.viewportCappedEndRow;
      }
      return x < this.startCol && y >= this.viewportCappedStartRow && x >= this.endCol && y <= this.viewportCappedEndRow;
    }
    return y > this.viewportStartRow && y < this.viewportEndRow || this.viewportStartRow === this.viewportEndRow && y === this.viewportStartRow && x >= this.startCol && x < this.endCol || this.viewportStartRow < this.viewportEndRow && y === this.viewportEndRow && x < this.endCol || this.viewportStartRow < this.viewportEndRow && y === this.viewportStartRow && x >= this.startCol;
  }
};
function createSelectionRenderModel() {
  return new SelectionRenderModel();
}

// vendor/xterm-addon-webgl/src/RenderModel.ts
var RENDER_MODEL_INDICIES_PER_CELL = 4;
var RENDER_MODEL_BG_OFFSET = 1;
var RENDER_MODEL_FG_OFFSET = 2;
var RENDER_MODEL_EXT_OFFSET = 3;
var COMBINED_CHAR_BIT_MASK = 2147483648;
var RenderModel = class {
  constructor() {
    this.cells = new Uint32Array(0);
    this.lineLengths = new Uint32Array(0);
    this.selection = createSelectionRenderModel();
  }
  resize(cols, rows) {
    const indexCount = cols * rows * RENDER_MODEL_INDICIES_PER_CELL;
    if (indexCount !== this.cells.length) {
      this.cells = new Uint32Array(indexCount);
      this.lineLengths = new Uint32Array(rows);
    }
  }
  clear() {
    this.cells.fill(0, 0);
    this.lineLengths.fill(0, 0);
  }
};

// vendor/xterm-addon-webgl/src/RectangleRenderer.ts
var vertexShaderSource2 = `#version 300 es
layout (location = ${0 /* POSITION */}) in vec2 a_position;
layout (location = ${1 /* SIZE */}) in vec2 a_size;
layout (location = ${2 /* COLOR */}) in vec4 a_color;
layout (location = ${3 /* UNIT_QUAD */}) in vec2 a_unitquad;

uniform mat4 u_projection;

out vec4 v_color;

void main() {
  vec2 zeroToOne = a_position + (a_unitquad * a_size);
  gl_Position = u_projection * vec4(zeroToOne, 0.0, 1.0);
  v_color = a_color;
}`;
var fragmentShaderSource = `#version 300 es
precision lowp float;

in vec4 v_color;

out vec4 outColor;

void main() {
  outColor = v_color;
}`;
var INDICES_PER_RECTANGLE = 8;
var BYTES_PER_RECTANGLE = INDICES_PER_RECTANGLE * Float32Array.BYTES_PER_ELEMENT;
var INITIAL_BUFFER_RECTANGLE_CAPACITY = 20 * INDICES_PER_RECTANGLE;
var Vertices = class {
  constructor() {
    this.attributes = new Float32Array(INITIAL_BUFFER_RECTANGLE_CAPACITY);
    this.count = 0;
  }
};
var $rgba = 0;
var $x1 = 0;
var $y1 = 0;
var $r2 = 0;
var $g2 = 0;
var $b2 = 0;
var $a2 = 0;
var RectangleRenderer = class extends Disposable {
  constructor(_terminal, _gl, _dimensions, _themeService) {
    super();
    this._terminal = _terminal;
    this._gl = _gl;
    this._dimensions = _dimensions;
    this._themeService = _themeService;
    this._vertices = new Vertices();
    this._verticesCursor = new Vertices();
    const gl = this._gl;
    this._program = throwIfFalsy(createProgram(gl, vertexShaderSource2, fragmentShaderSource));
    this._register(toDisposable(() => gl.deleteProgram(this._program)));
    this._projectionLocation = throwIfFalsy(gl.getUniformLocation(this._program, "u_projection"));
    this._vertexArrayObject = gl.createVertexArray();
    gl.bindVertexArray(this._vertexArrayObject);
    const unitQuadVertices = new Float32Array([0, 0, 1, 0, 0, 1, 1, 1]);
    const unitQuadVerticesBuffer = gl.createBuffer();
    this._register(toDisposable(() => gl.deleteBuffer(unitQuadVerticesBuffer)));
    gl.bindBuffer(gl.ARRAY_BUFFER, unitQuadVerticesBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, unitQuadVertices, gl.STATIC_DRAW);
    gl.enableVertexAttribArray(3 /* UNIT_QUAD */);
    gl.vertexAttribPointer(3 /* UNIT_QUAD */, 2, this._gl.FLOAT, false, 0, 0);
    const unitQuadElementIndices = new Uint8Array([0, 1, 2, 3]);
    const elementIndicesBuffer = gl.createBuffer();
    this._register(toDisposable(() => gl.deleteBuffer(elementIndicesBuffer)));
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, elementIndicesBuffer);
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, unitQuadElementIndices, gl.STATIC_DRAW);
    this._attributesBuffer = throwIfFalsy(gl.createBuffer());
    this._register(toDisposable(() => gl.deleteBuffer(this._attributesBuffer)));
    gl.bindBuffer(gl.ARRAY_BUFFER, this._attributesBuffer);
    gl.enableVertexAttribArray(0 /* POSITION */);
    gl.vertexAttribPointer(0 /* POSITION */, 2, gl.FLOAT, false, BYTES_PER_RECTANGLE, 0);
    gl.vertexAttribDivisor(0 /* POSITION */, 1);
    gl.enableVertexAttribArray(1 /* SIZE */);
    gl.vertexAttribPointer(1 /* SIZE */, 2, gl.FLOAT, false, BYTES_PER_RECTANGLE, 2 * Float32Array.BYTES_PER_ELEMENT);
    gl.vertexAttribDivisor(1 /* SIZE */, 1);
    gl.enableVertexAttribArray(2 /* COLOR */);
    gl.vertexAttribPointer(2 /* COLOR */, 4, gl.FLOAT, false, BYTES_PER_RECTANGLE, 4 * Float32Array.BYTES_PER_ELEMENT);
    gl.vertexAttribDivisor(2 /* COLOR */, 1);
    this._updateCachedColors(_themeService.colors);
    this._register(this._themeService.onChangeColors((e) => {
      this._updateCachedColors(e);
      this._updateViewportRectangle();
    }));
  }
  renderBackgrounds() {
    this._renderVertices(this._vertices);
  }
  renderCursor() {
    this._renderVertices(this._verticesCursor);
  }
  _renderVertices(vertices) {
    const gl = this._gl;
    gl.useProgram(this._program);
    gl.bindVertexArray(this._vertexArrayObject);
    gl.uniformMatrix4fv(this._projectionLocation, false, PROJECTION_MATRIX);
    gl.bindBuffer(gl.ARRAY_BUFFER, this._attributesBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, vertices.attributes, gl.DYNAMIC_DRAW);
    gl.drawElementsInstanced(this._gl.TRIANGLE_STRIP, 4, gl.UNSIGNED_BYTE, 0, vertices.count);
  }
  handleResize() {
    this._updateViewportRectangle();
  }
  setDimensions(dimensions) {
    this._dimensions = dimensions;
  }
  _updateCachedColors(colors) {
    this._bgFloat = this._colorToFloat32Array(colors.background);
    this._cursorFloat = this._colorToFloat32Array(colors.cursor);
  }
  _updateViewportRectangle() {
    this._addRectangleFloat(
      this._vertices.attributes,
      0,
      0,
      0,
      this._terminal.cols * this._dimensions.device.cell.width,
      this._terminal.rows * this._dimensions.device.cell.height,
      this._bgFloat
    );
  }
  updateBackgrounds(model) {
    const terminal = this._terminal;
    const vertices = this._vertices;
    let rectangleCount = 1;
    let y;
    let x;
    let currentStartX;
    let currentBg;
    let currentFg;
    let currentInverse;
    let modelIndex;
    let bg;
    let fg;
    let inverse;
    let offset;
    for (y = 0; y < terminal.rows; y++) {
      currentStartX = -1;
      currentBg = 0;
      currentFg = 0;
      currentInverse = false;
      for (x = 0; x < terminal.cols; x++) {
        modelIndex = (y * terminal.cols + x) * RENDER_MODEL_INDICIES_PER_CELL;
        bg = model.cells[modelIndex + RENDER_MODEL_BG_OFFSET];
        fg = model.cells[modelIndex + RENDER_MODEL_FG_OFFSET];
        inverse = !!(fg & 67108864 /* INVERSE */);
        if (bg !== currentBg || fg !== currentFg && (currentInverse || inverse)) {
          if (currentBg !== 0 || currentInverse && currentFg !== 0) {
            offset = rectangleCount++ * INDICES_PER_RECTANGLE;
            this._updateRectangle(vertices, offset, currentFg, currentBg, currentStartX, x, y);
          }
          currentStartX = x;
          currentBg = bg;
          currentFg = fg;
          currentInverse = inverse;
        }
      }
      if (currentBg !== 0 || currentInverse && currentFg !== 0) {
        offset = rectangleCount++ * INDICES_PER_RECTANGLE;
        this._updateRectangle(vertices, offset, currentFg, currentBg, currentStartX, terminal.cols, y);
      }
    }
    vertices.count = rectangleCount;
  }
  updateCursor(model) {
    const vertices = this._verticesCursor;
    const cursor = model.cursor;
    if (!cursor || cursor.style === "block") {
      vertices.count = 0;
      return;
    }
    let offset;
    let rectangleCount = 0;
    if (cursor.style === "bar" || cursor.style === "outline") {
      offset = rectangleCount++ * INDICES_PER_RECTANGLE;
      this._addRectangleFloat(
        vertices.attributes,
        offset,
        cursor.x * this._dimensions.device.cell.width,
        cursor.y * this._dimensions.device.cell.height,
        cursor.style === "bar" ? cursor.dpr * cursor.cursorWidth : cursor.dpr,
        this._dimensions.device.cell.height,
        this._cursorFloat
      );
    }
    if (cursor.style === "underline" || cursor.style === "outline") {
      offset = rectangleCount++ * INDICES_PER_RECTANGLE;
      this._addRectangleFloat(
        vertices.attributes,
        offset,
        cursor.x * this._dimensions.device.cell.width,
        (cursor.y + 1) * this._dimensions.device.cell.height - cursor.dpr,
        cursor.width * this._dimensions.device.cell.width,
        cursor.dpr,
        this._cursorFloat
      );
    }
    if (cursor.style === "outline") {
      offset = rectangleCount++ * INDICES_PER_RECTANGLE;
      this._addRectangleFloat(
        vertices.attributes,
        offset,
        cursor.x * this._dimensions.device.cell.width,
        cursor.y * this._dimensions.device.cell.height,
        cursor.width * this._dimensions.device.cell.width,
        cursor.dpr,
        this._cursorFloat
      );
      offset = rectangleCount++ * INDICES_PER_RECTANGLE;
      this._addRectangleFloat(
        vertices.attributes,
        offset,
        (cursor.x + cursor.width) * this._dimensions.device.cell.width - cursor.dpr,
        cursor.y * this._dimensions.device.cell.height,
        cursor.dpr,
        this._dimensions.device.cell.height,
        this._cursorFloat
      );
    }
    vertices.count = rectangleCount;
  }
  _updateRectangle(vertices, offset, fg, bg, startX, endX, y) {
    if (fg & 67108864 /* INVERSE */) {
      switch (fg & 50331648 /* CM_MASK */) {
        case 16777216 /* CM_P16 */:
        case 33554432 /* CM_P256 */:
          $rgba = this._themeService.colors.ansi[fg & 255 /* PCOLOR_MASK */].rgba;
          break;
        case 50331648 /* CM_RGB */:
          $rgba = (fg & 16777215 /* RGB_MASK */) << 8;
          break;
        case 0 /* CM_DEFAULT */:
        default:
          $rgba = this._themeService.colors.foreground.rgba;
      }
    } else {
      switch (bg & 50331648 /* CM_MASK */) {
        case 16777216 /* CM_P16 */:
        case 33554432 /* CM_P256 */:
          $rgba = this._themeService.colors.ansi[bg & 255 /* PCOLOR_MASK */].rgba;
          break;
        case 50331648 /* CM_RGB */:
          $rgba = (bg & 16777215 /* RGB_MASK */) << 8;
          break;
        case 0 /* CM_DEFAULT */:
        default:
          $rgba = this._themeService.colors.background.rgba;
      }
    }
    if (vertices.attributes.length < offset + INDICES_PER_RECTANGLE) {
      vertices.attributes = expandFloat32Array(vertices.attributes, (this._terminal.rows * this._terminal.cols + 1) * INDICES_PER_RECTANGLE);
    }
    $x1 = startX * this._dimensions.device.cell.width;
    $y1 = y * this._dimensions.device.cell.height;
    $r2 = ($rgba >> 24 & 255) / 255;
    $g2 = ($rgba >> 16 & 255) / 255;
    $b2 = ($rgba >> 8 & 255) / 255;
    $a2 = 1;
    this._addRectangle(vertices.attributes, offset, $x1, $y1, (endX - startX) * this._dimensions.device.cell.width, this._dimensions.device.cell.height, $r2, $g2, $b2, $a2);
  }
  _addRectangle(array, offset, x1, y1, width, height, r, g, b, a) {
    array[offset] = x1 / this._dimensions.device.canvas.width;
    array[offset + 1] = y1 / this._dimensions.device.canvas.height;
    array[offset + 2] = width / this._dimensions.device.canvas.width;
    array[offset + 3] = height / this._dimensions.device.canvas.height;
    array[offset + 4] = r;
    array[offset + 5] = g;
    array[offset + 6] = b;
    array[offset + 7] = a;
  }
  _addRectangleFloat(array, offset, x1, y1, width, height, color2) {
    array[offset] = x1 / this._dimensions.device.canvas.width;
    array[offset + 1] = y1 / this._dimensions.device.canvas.height;
    array[offset + 2] = width / this._dimensions.device.canvas.width;
    array[offset + 3] = height / this._dimensions.device.canvas.height;
    array[offset + 4] = color2[0];
    array[offset + 5] = color2[1];
    array[offset + 6] = color2[2];
    array[offset + 7] = color2[3];
  }
  _colorToFloat32Array(color2) {
    return new Float32Array([
      (color2.rgba >> 24 & 255) / 255,
      (color2.rgba >> 16 & 255) / 255,
      (color2.rgba >> 8 & 255) / 255,
      (color2.rgba & 255) / 255
    ]);
  }
};

// node_modules/@xterm/xterm/src/browser/renderer/shared/Constants.ts
var INVERTED_DEFAULT_COLOR = 257;

// vendor/xterm-addon-webgl/src/renderLayer/BaseRenderLayer.ts
var BaseRenderLayer = class extends Disposable {
  constructor(terminal, _container, id2, zIndex, _alpha, _coreBrowserService, _optionsService, _themeService) {
    super();
    this._container = _container;
    this._alpha = _alpha;
    this._coreBrowserService = _coreBrowserService;
    this._optionsService = _optionsService;
    this._themeService = _themeService;
    this._deviceCharWidth = 0;
    this._deviceCharHeight = 0;
    this._deviceCellWidth = 0;
    this._deviceCellHeight = 0;
    this._deviceCharLeft = 0;
    this._deviceCharTop = 0;
    this._canvas = this._coreBrowserService.mainDocument.createElement("canvas");
    this._canvas.classList.add(`xterm-${id2}-layer`);
    this._canvas.style.zIndex = zIndex.toString();
    this._initCanvas();
    this._container.appendChild(this._canvas);
    this._register(this._themeService.onChangeColors((e) => {
      this._refreshCharAtlas(terminal, e);
      this.reset(terminal);
    }));
    this._register(toDisposable(() => {
      this._canvas.remove();
    }));
  }
  _initCanvas() {
    this._ctx = throwIfFalsy(this._canvas.getContext("2d", { alpha: this._alpha }));
    if (!this._alpha) {
      this._clearAll();
    }
  }
  handleBlur(terminal) {
  }
  handleFocus(terminal) {
  }
  handleCursorMove(terminal) {
  }
  handleGridChanged(terminal, startRow, endRow) {
  }
  handleSelectionChanged(terminal, start, end, columnSelectMode = false) {
  }
  _setTransparency(terminal, alpha) {
    if (alpha === this._alpha) {
      return;
    }
    const oldCanvas = this._canvas;
    this._alpha = alpha;
    this._canvas = this._canvas.cloneNode();
    this._initCanvas();
    this._container.replaceChild(this._canvas, oldCanvas);
    this._refreshCharAtlas(terminal, this._themeService.colors);
    this.handleGridChanged(terminal, 0, terminal.rows - 1);
  }
  /**
   * Refreshes the char atlas, aquiring a new one if necessary.
   * @param terminal The terminal.
   * @param colorSet The color set to use for the char atlas.
   */
  _refreshCharAtlas(terminal, colorSet) {
    if (this._deviceCharWidth <= 0 && this._deviceCharHeight <= 0) {
      return;
    }
    this._charAtlas = acquireTextureAtlas(terminal, this._optionsService.rawOptions, colorSet, this._deviceCellWidth, this._deviceCellHeight, this._deviceCharWidth, this._deviceCharHeight, this._coreBrowserService.dpr, 2048);
    this._charAtlas.warmUp();
  }
  resize(terminal, dim) {
    this._deviceCellWidth = dim.device.cell.width;
    this._deviceCellHeight = dim.device.cell.height;
    this._deviceCharWidth = dim.device.char.width;
    this._deviceCharHeight = dim.device.char.height;
    this._deviceCharLeft = dim.device.char.left;
    this._deviceCharTop = dim.device.char.top;
    this._canvas.width = dim.device.canvas.width;
    this._canvas.height = dim.device.canvas.height;
    this._canvas.style.width = `${dim.css.canvas.width}px`;
    this._canvas.style.height = `${dim.css.canvas.height}px`;
    if (!this._alpha) {
      this._clearAll();
    }
    this._refreshCharAtlas(terminal, this._themeService.colors);
  }
  /**
   * Fills a 1px line (2px on HDPI) at the bottom of the cell. This uses the
   * existing fillStyle on the context.
   * @param x The column to fill.
   * @param y The row to fill.
   */
  _fillBottomLineAtCells(x, y, width = 1) {
    this._ctx.fillRect(
      x * this._deviceCellWidth,
      (y + 1) * this._deviceCellHeight - this._coreBrowserService.dpr - 1,
      width * this._deviceCellWidth,
      this._coreBrowserService.dpr
    );
  }
  /**
   * Clears the entire canvas.
   */
  _clearAll() {
    if (this._alpha) {
      this._ctx.clearRect(0, 0, this._canvas.width, this._canvas.height);
    } else {
      this._ctx.fillStyle = this._themeService.colors.background.css;
      this._ctx.fillRect(0, 0, this._canvas.width, this._canvas.height);
    }
  }
  /**
   * Clears 1+ cells completely.
   * @param x The column to start at.
   * @param y The row to start at.
   * @param width The number of columns to clear.
   * @param height The number of rows to clear.
   */
  _clearCells(x, y, width, height) {
    if (this._alpha) {
      this._ctx.clearRect(
        x * this._deviceCellWidth,
        y * this._deviceCellHeight,
        width * this._deviceCellWidth,
        height * this._deviceCellHeight
      );
    } else {
      this._ctx.fillStyle = this._themeService.colors.background.css;
      this._ctx.fillRect(
        x * this._deviceCellWidth,
        y * this._deviceCellHeight,
        width * this._deviceCellWidth,
        height * this._deviceCellHeight
      );
    }
  }
  /**
   * Draws a truecolor character at the cell. The character will be clipped to
   * ensure that it fits with the cell, including the cell to the right if it's
   * a wide character. This uses the existing fillStyle on the context.
   * @param terminal The terminal.
   * @param cell The cell data for the character to draw.
   * @param x The column to draw at.
   * @param y The row to draw at.
   */
  _fillCharTrueColor(terminal, cell, x, y) {
    this._ctx.font = this._getFont(terminal, false, false);
    this._ctx.textBaseline = TEXT_BASELINE;
    this._clipCell(x, y, cell.getWidth());
    this._ctx.fillText(
      cell.getChars(),
      x * this._deviceCellWidth + this._deviceCharLeft,
      y * this._deviceCellHeight + this._deviceCharTop + this._deviceCharHeight
    );
  }
  /**
   * Clips a cell to ensure no pixels will be drawn outside of it.
   * @param x The column to clip.
   * @param y The row to clip.
   * @param width The number of columns to clip.
   */
  _clipCell(x, y, width) {
    this._ctx.beginPath();
    this._ctx.rect(
      x * this._deviceCellWidth,
      y * this._deviceCellHeight,
      width * this._deviceCellWidth,
      this._deviceCellHeight
    );
    this._ctx.clip();
  }
  /**
   * Gets the current font.
   * @param terminal The terminal.
   * @param isBold If we should use the bold fontWeight.
   */
  _getFont(terminal, isBold, isItalic) {
    const fontWeight = isBold ? terminal.options.fontWeightBold : terminal.options.fontWeight;
    const fontStyle = isItalic ? "italic" : "";
    return `${fontStyle} ${fontWeight} ${terminal.options.fontSize * this._coreBrowserService.dpr}px ${terminal.options.fontFamily}`;
  }
};

// vendor/xterm-addon-webgl/src/renderLayer/LinkRenderLayer.ts
var LinkRenderLayer = class extends BaseRenderLayer {
  constructor(container, zIndex, terminal, linkifier2, coreBrowserService, optionsService, themeService) {
    super(terminal, container, "link", zIndex, true, coreBrowserService, optionsService, themeService);
    this._register(linkifier2.onShowLinkUnderline((e) => this._handleShowLinkUnderline(e)));
    this._register(linkifier2.onHideLinkUnderline((e) => this._handleHideLinkUnderline(e)));
  }
  resize(terminal, dim) {
    super.resize(terminal, dim);
    this._state = void 0;
  }
  reset(terminal) {
    this._clearCurrentLink();
  }
  _clearCurrentLink() {
    if (this._state) {
      this._clearCells(this._state.x1, this._state.y1, this._state.cols - this._state.x1, 1);
      const middleRowCount = this._state.y2 - this._state.y1 - 1;
      if (middleRowCount > 0) {
        this._clearCells(0, this._state.y1 + 1, this._state.cols, middleRowCount);
      }
      this._clearCells(0, this._state.y2, this._state.x2, 1);
      this._state = void 0;
    }
  }
  _handleShowLinkUnderline(e) {
    if (e.fg === INVERTED_DEFAULT_COLOR) {
      this._ctx.fillStyle = this._themeService.colors.background.css;
    } else if (e.fg !== void 0 && is256Color(e.fg)) {
      this._ctx.fillStyle = this._themeService.colors.ansi[e.fg].css;
    } else {
      this._ctx.fillStyle = this._themeService.colors.foreground.css;
    }
    if (e.y1 === e.y2) {
      this._fillBottomLineAtCells(e.x1, e.y1, e.x2 - e.x1);
    } else {
      this._fillBottomLineAtCells(e.x1, e.y1, e.cols - e.x1);
      for (let y = e.y1 + 1; y < e.y2; y++) {
        this._fillBottomLineAtCells(0, y, e.cols);
      }
      this._fillBottomLineAtCells(0, e.y2, e.x2);
    }
    this._state = e;
  }
  _handleHideLinkUnderline(e) {
    this._clearCurrentLink();
  }
};

// node_modules/@xterm/xterm/src/vs/base/browser/window.ts
function ensureCodeWindow(targetWindow, fallbackWindowId) {
}
var mainWindow = typeof window === "object" ? window : globalThis;

// node_modules/@xterm/xterm/src/vs/base/browser/browser.ts
var _WindowManager = class _WindowManager {
  constructor() {
    // --- Zoom Level
    this.mapWindowIdToZoomLevel = /* @__PURE__ */ new Map();
    this._onDidChangeZoomLevel = new Emitter();
    this.onDidChangeZoomLevel = this._onDidChangeZoomLevel.event;
    // --- Zoom Factor
    this.mapWindowIdToZoomFactor = /* @__PURE__ */ new Map();
    // --- Fullscreen
    this._onDidChangeFullscreen = new Emitter();
    this.onDidChangeFullscreen = this._onDidChangeFullscreen.event;
    this.mapWindowIdToFullScreen = /* @__PURE__ */ new Map();
  }
  getZoomLevel(targetWindow) {
    return this.mapWindowIdToZoomLevel.get(this.getWindowId(targetWindow)) ?? 0;
  }
  setZoomLevel(zoomLevel, targetWindow) {
    if (this.getZoomLevel(targetWindow) === zoomLevel) {
      return;
    }
    const targetWindowId = this.getWindowId(targetWindow);
    this.mapWindowIdToZoomLevel.set(targetWindowId, zoomLevel);
    this._onDidChangeZoomLevel.fire(targetWindowId);
  }
  getZoomFactor(targetWindow) {
    return this.mapWindowIdToZoomFactor.get(this.getWindowId(targetWindow)) ?? 1;
  }
  setZoomFactor(zoomFactor, targetWindow) {
    this.mapWindowIdToZoomFactor.set(this.getWindowId(targetWindow), zoomFactor);
  }
  setFullscreen(fullscreen, targetWindow) {
    if (this.isFullscreen(targetWindow) === fullscreen) {
      return;
    }
    const windowId = this.getWindowId(targetWindow);
    this.mapWindowIdToFullScreen.set(windowId, fullscreen);
    this._onDidChangeFullscreen.fire(windowId);
  }
  isFullscreen(targetWindow) {
    return !!this.mapWindowIdToFullScreen.get(this.getWindowId(targetWindow));
  }
  getWindowId(targetWindow) {
    return targetWindow.vscodeWindowId;
  }
};
_WindowManager.INSTANCE = new _WindowManager();
var WindowManager = _WindowManager;
function addMatchMediaChangeListener(targetWindow, query, callback) {
  if (typeof query === "string") {
    query = targetWindow.matchMedia(query);
  }
  query.addEventListener("change", callback);
}
var onDidChangeZoomLevel = WindowManager.INSTANCE.onDidChangeZoomLevel;
var onDidChangeFullscreen = WindowManager.INSTANCE.onDidChangeFullscreen;
var userAgent2 = typeof navigator === "object" ? navigator.userAgent : "";
var isFirefox2 = userAgent2.indexOf("Firefox") >= 0;
var isWebKit = userAgent2.indexOf("AppleWebKit") >= 0;
var isChrome = userAgent2.indexOf("Chrome") >= 0;
var isSafari2 = !isChrome && userAgent2.indexOf("Safari") >= 0;
var isElectron = userAgent2.indexOf("Electron/") >= 0;
var isAndroid = userAgent2.indexOf("Android") >= 0;
var standalone = false;
if (typeof mainWindow.matchMedia === "function") {
  const standaloneMatchMedia = mainWindow.matchMedia("(display-mode: standalone) or (display-mode: window-controls-overlay)");
  const fullScreenMatchMedia = mainWindow.matchMedia("(display-mode: fullscreen)");
  standalone = standaloneMatchMedia.matches;
  addMatchMediaChangeListener(mainWindow, standaloneMatchMedia, ({ matches }) => {
    if (standalone && fullScreenMatchMedia.matches) {
      return;
    }
    standalone = matches;
  });
}
function isStandalone() {
  return standalone;
}

// node_modules/@xterm/xterm/src/vs/base/common/platform.ts
var LANGUAGE_DEFAULT = "en";
var _isWindows = false;
var _isMacintosh = false;
var _isLinux = false;
var _isLinuxSnap = false;
var _isNative = false;
var _isWeb = false;
var _isElectron = false;
var _isIOS = false;
var _isCI = false;
var _isMobile = false;
var _locale = void 0;
var _language = LANGUAGE_DEFAULT;
var _platformLocale = LANGUAGE_DEFAULT;
var _translationsConfigFile = void 0;
var _userAgent = void 0;
var $globalThis = globalThis;
var nodeProcess = void 0;
if (typeof $globalThis.vscode !== "undefined" && typeof $globalThis.vscode.process !== "undefined") {
  nodeProcess = $globalThis.vscode.process;
} else if (typeof process !== "undefined" && typeof process?.versions?.node === "string") {
  nodeProcess = process;
}
var isElectronProcess = typeof nodeProcess?.versions?.electron === "string";
var isElectronRenderer = isElectronProcess && nodeProcess?.type === "renderer";
if (typeof nodeProcess === "object") {
  _isWindows = nodeProcess.platform === "win32";
  _isMacintosh = nodeProcess.platform === "darwin";
  _isLinux = nodeProcess.platform === "linux";
  _isLinuxSnap = _isLinux && !!nodeProcess.env["SNAP"] && !!nodeProcess.env["SNAP_REVISION"];
  _isElectron = isElectronProcess;
  _isCI = !!nodeProcess.env["CI"] || !!nodeProcess.env["BUILD_ARTIFACTSTAGINGDIRECTORY"];
  _locale = LANGUAGE_DEFAULT;
  _language = LANGUAGE_DEFAULT;
  const rawNlsConfig = nodeProcess.env["VSCODE_NLS_CONFIG"];
  if (rawNlsConfig) {
    try {
      const nlsConfig = JSON.parse(rawNlsConfig);
      _locale = nlsConfig.userLocale;
      _platformLocale = nlsConfig.osLocale;
      _language = nlsConfig.resolvedLanguage || LANGUAGE_DEFAULT;
      _translationsConfigFile = nlsConfig.languagePack?.translationsConfigFile;
    } catch (e) {
    }
  }
  _isNative = true;
} else if (typeof navigator === "object" && !isElectronRenderer) {
  _userAgent = navigator.userAgent;
  _isWindows = _userAgent.indexOf("Windows") >= 0;
  _isMacintosh = _userAgent.indexOf("Macintosh") >= 0;
  _isIOS = (_userAgent.indexOf("Macintosh") >= 0 || _userAgent.indexOf("iPad") >= 0 || _userAgent.indexOf("iPhone") >= 0) && !!navigator.maxTouchPoints && navigator.maxTouchPoints > 0;
  _isLinux = _userAgent.indexOf("Linux") >= 0;
  _isMobile = _userAgent?.indexOf("Mobi") >= 0;
  _isWeb = true;
  _language = globalThis._VSCODE_NLS_LANGUAGE || LANGUAGE_DEFAULT;
  _locale = navigator.language.toLowerCase();
  _platformLocale = _locale;
} else {
  console.error("Unable to resolve platform.");
}
var _platform = 0 /* Web */;
if (_isMacintosh) {
  _platform = 1 /* Mac */;
} else if (_isWindows) {
  _platform = 3 /* Windows */;
} else if (_isLinux) {
  _platform = 2 /* Linux */;
}
var isMacintosh = _isMacintosh;
var isNative = _isNative;
var isWebWorker = _isWeb && typeof $globalThis.importScripts === "function";
var webWorkerOrigin = isWebWorker ? $globalThis.origin : void 0;
var userAgent3 = _userAgent;
var language = _language;
var Language;
((Language2) => {
  function value() {
    return language;
  }
  Language2.value = value;
  function isDefaultVariant() {
    if (language.length === 2) {
      return language === "en";
    } else if (language.length >= 3) {
      return language[0] === "e" && language[1] === "n" && language[2] === "-";
    } else {
      return false;
    }
  }
  Language2.isDefaultVariant = isDefaultVariant;
  function isDefault() {
    return language === "en";
  }
  Language2.isDefault = isDefault;
})(Language || (Language = {}));
var setTimeout0IsFaster = typeof $globalThis.postMessage === "function" && !$globalThis.importScripts;
var setTimeout0 = (() => {
  if (setTimeout0IsFaster) {
    const pending = [];
    $globalThis.addEventListener("message", (e) => {
      if (e.data && e.data.vscodeScheduleAsyncWork) {
        for (let i = 0, len = pending.length; i < len; i++) {
          const candidate = pending[i];
          if (candidate.id === e.data.vscodeScheduleAsyncWork) {
            pending.splice(i, 1);
            candidate.callback();
            return;
          }
        }
      }
    });
    let lastId = 0;
    return (callback) => {
      const myId = ++lastId;
      pending.push({
        id: myId,
        callback
      });
      $globalThis.postMessage({ vscodeScheduleAsyncWork: myId }, "*");
    };
  }
  return (callback) => setTimeout(callback);
})();
var isChrome2 = !!(userAgent3 && userAgent3.indexOf("Chrome") >= 0);
var isFirefox3 = !!(userAgent3 && userAgent3.indexOf("Firefox") >= 0);
var isSafari3 = !!(!isChrome2 && (userAgent3 && userAgent3.indexOf("Safari") >= 0));
var isEdge = !!(userAgent3 && userAgent3.indexOf("Edg/") >= 0);
var isAndroid2 = !!(userAgent3 && userAgent3.indexOf("Android") >= 0);

// node_modules/@xterm/xterm/src/vs/base/browser/canIUse.ts
var safeNavigator = typeof navigator === "object" ? navigator : {};
var BrowserFeatures = {
  clipboard: {
    writeText: isNative || document.queryCommandSupported && document.queryCommandSupported("copy") || !!(safeNavigator && safeNavigator.clipboard && safeNavigator.clipboard.writeText),
    readText: isNative || !!(safeNavigator && safeNavigator.clipboard && safeNavigator.clipboard.readText)
  },
  keyboard: (() => {
    if (isNative || isStandalone()) {
      return 0 /* Always */;
    }
    if (safeNavigator.keyboard || isSafari2) {
      return 1 /* FullScreen */;
    }
    return 2 /* None */;
  })(),
  // 'ontouchstart' in window always evaluates to true with typescript's modern typings. This causes `window` to be
  // `never` later in `window.navigator`. That's why we need the explicit `window as Window` cast
  touch: "ontouchstart" in mainWindow || safeNavigator.maxTouchPoints > 0,
  pointerEvents: mainWindow.PointerEvent && ("ontouchstart" in mainWindow || navigator.maxTouchPoints > 0)
};

// node_modules/@xterm/xterm/src/vs/base/common/keyCodes.ts
var KeyCodeStrMap = class {
  constructor() {
    this._keyCodeToStr = [];
    this._strToKeyCode = /* @__PURE__ */ Object.create(null);
  }
  define(keyCode, str) {
    this._keyCodeToStr[keyCode] = str;
    this._strToKeyCode[str.toLowerCase()] = keyCode;
  }
  keyCodeToStr(keyCode) {
    return this._keyCodeToStr[keyCode];
  }
  strToKeyCode(str) {
    return this._strToKeyCode[str.toLowerCase()] || 0 /* Unknown */;
  }
};
var uiMap = new KeyCodeStrMap();
var userSettingsUSMap = new KeyCodeStrMap();
var userSettingsGeneralMap = new KeyCodeStrMap();
var EVENT_KEY_CODE_MAP = new Array(230);
var KeyCodeUtils;
((KeyCodeUtils2) => {
  function toString(keyCode) {
    return uiMap.keyCodeToStr(keyCode);
  }
  KeyCodeUtils2.toString = toString;
  function fromString(key) {
    return uiMap.strToKeyCode(key);
  }
  KeyCodeUtils2.fromString = fromString;
  function toUserSettingsUS(keyCode) {
    return userSettingsUSMap.keyCodeToStr(keyCode);
  }
  KeyCodeUtils2.toUserSettingsUS = toUserSettingsUS;
  function toUserSettingsGeneral(keyCode) {
    return userSettingsGeneralMap.keyCodeToStr(keyCode);
  }
  KeyCodeUtils2.toUserSettingsGeneral = toUserSettingsGeneral;
  function fromUserSettings(key) {
    return userSettingsUSMap.strToKeyCode(key) || userSettingsGeneralMap.strToKeyCode(key);
  }
  KeyCodeUtils2.fromUserSettings = fromUserSettings;
  function toElectronAccelerator(keyCode) {
    if (keyCode >= 98 /* Numpad0 */ && keyCode <= 113 /* NumpadDivide */) {
      return null;
    }
    switch (keyCode) {
      case 16 /* UpArrow */:
        return "Up";
      case 18 /* DownArrow */:
        return "Down";
      case 15 /* LeftArrow */:
        return "Left";
      case 17 /* RightArrow */:
        return "Right";
    }
    return uiMap.keyCodeToStr(keyCode);
  }
  KeyCodeUtils2.toElectronAccelerator = toElectronAccelerator;
})(KeyCodeUtils || (KeyCodeUtils = {}));

// node_modules/@xterm/xterm/src/vs/base/browser/keyboardEvent.ts
var ctrlKeyMod = isMacintosh ? 256 /* WinCtrl */ : 2048 /* CtrlCmd */;
var altKeyMod = 512 /* Alt */;
var shiftKeyMod = 1024 /* Shift */;
var metaKeyMod = isMacintosh ? 2048 /* CtrlCmd */ : 256 /* WinCtrl */;

// node_modules/@xterm/xterm/src/vs/base/common/cancellation.ts
var shortcutEvent = Object.freeze(function(callback, context) {
  const handle = setTimeout(callback.bind(context), 0);
  return { dispose() {
    clearTimeout(handle);
  } };
});
var CancellationToken;
((CancellationToken3) => {
  function isCancellationToken(thing) {
    if (thing === CancellationToken3.None || thing === CancellationToken3.Cancelled) {
      return true;
    }
    if (thing instanceof MutableToken) {
      return true;
    }
    if (!thing || typeof thing !== "object") {
      return false;
    }
    return typeof thing.isCancellationRequested === "boolean" && typeof thing.onCancellationRequested === "function";
  }
  CancellationToken3.isCancellationToken = isCancellationToken;
  CancellationToken3.None = Object.freeze({
    isCancellationRequested: false,
    onCancellationRequested: Event.None
  });
  CancellationToken3.Cancelled = Object.freeze({
    isCancellationRequested: true,
    onCancellationRequested: shortcutEvent
  });
})(CancellationToken || (CancellationToken = {}));
var MutableToken = class {
  constructor() {
    this._isCancelled = false;
    this._emitter = null;
  }
  cancel() {
    if (!this._isCancelled) {
      this._isCancelled = true;
      if (this._emitter) {
        this._emitter.fire(void 0);
        this.dispose();
      }
    }
  }
  get isCancellationRequested() {
    return this._isCancelled;
  }
  get onCancellationRequested() {
    if (this._isCancelled) {
      return shortcutEvent;
    }
    if (!this._emitter) {
      this._emitter = new Emitter();
    }
    return this._emitter.event;
  }
  dispose() {
    if (this._emitter) {
      this._emitter.dispose();
      this._emitter = null;
    }
  }
};

// node_modules/@xterm/xterm/src/vs/base/common/symbols.ts
var MicrotaskDelay = Symbol("MicrotaskDelay");

// node_modules/@xterm/xterm/src/vs/base/common/async.ts
var runWhenGlobalIdle;
var _runWhenIdle;
(function() {
  if (typeof globalThis.requestIdleCallback !== "function" || typeof globalThis.cancelIdleCallback !== "function") {
    _runWhenIdle = (_targetWindow, runner) => {
      setTimeout0(() => {
        if (disposed) {
          return;
        }
        const end = Date.now() + 15;
        const deadline = {
          didTimeout: true,
          timeRemaining() {
            return Math.max(0, end - Date.now());
          }
        };
        runner(Object.freeze(deadline));
      });
      let disposed = false;
      return {
        dispose() {
          if (disposed) {
            return;
          }
          disposed = true;
        }
      };
    };
  } else {
    _runWhenIdle = (targetWindow, runner, timeout) => {
      const handle = targetWindow.requestIdleCallback(runner, typeof timeout === "number" ? { timeout } : void 0);
      let disposed = false;
      return {
        dispose() {
          if (disposed) {
            return;
          }
          disposed = true;
          targetWindow.cancelIdleCallback(handle);
        }
      };
    };
  }
  runWhenGlobalIdle = (runner) => _runWhenIdle(globalThis, runner);
})();
var Promises;
((Promises2) => {
  async function settled(promises) {
    let firstError = void 0;
    const result = await Promise.all(promises.map((promise) => promise.then((value) => value, (error) => {
      if (!firstError) {
        firstError = error;
      }
      return void 0;
    })));
    if (typeof firstError !== "undefined") {
      throw firstError;
    }
    return result;
  }
  Promises2.settled = settled;
  function withAsyncBody(bodyFn) {
    return new Promise(async (resolve, reject) => {
      try {
        await bodyFn(resolve, reject);
      } catch (error) {
        reject(error);
      }
    });
  }
  Promises2.withAsyncBody = withAsyncBody;
})(Promises || (Promises = {}));
var _AsyncIterableObject = class _AsyncIterableObject {
  static fromArray(items) {
    return new _AsyncIterableObject((writer) => {
      writer.emitMany(items);
    });
  }
  static fromPromise(promise) {
    return new _AsyncIterableObject(async (emitter) => {
      emitter.emitMany(await promise);
    });
  }
  static fromPromises(promises) {
    return new _AsyncIterableObject(async (emitter) => {
      await Promise.all(promises.map(async (p) => emitter.emitOne(await p)));
    });
  }
  static merge(iterables) {
    return new _AsyncIterableObject(async (emitter) => {
      await Promise.all(iterables.map(async (iterable) => {
        for await (const item of iterable) {
          emitter.emitOne(item);
        }
      }));
    });
  }
  constructor(executor, onReturn) {
    this._state = 0 /* Initial */;
    this._results = [];
    this._error = null;
    this._onReturn = onReturn;
    this._onStateChanged = new Emitter();
    queueMicrotask(async () => {
      const writer = {
        emitOne: (item) => this.emitOne(item),
        emitMany: (items) => this.emitMany(items),
        reject: (error) => this.reject(error)
      };
      try {
        await Promise.resolve(executor(writer));
        this.resolve();
      } catch (err) {
        this.reject(err);
      } finally {
        writer.emitOne = void 0;
        writer.emitMany = void 0;
        writer.reject = void 0;
      }
    });
  }
  [Symbol.asyncIterator]() {
    let i = 0;
    return {
      next: async () => {
        do {
          if (this._state === 2 /* DoneError */) {
            throw this._error;
          }
          if (i < this._results.length) {
            return { done: false, value: this._results[i++] };
          }
          if (this._state === 1 /* DoneOK */) {
            return { done: true, value: void 0 };
          }
          await Event.toPromise(this._onStateChanged.event);
        } while (true);
      },
      return: async () => {
        this._onReturn?.();
        return { done: true, value: void 0 };
      }
    };
  }
  static map(iterable, mapFn) {
    return new _AsyncIterableObject(async (emitter) => {
      for await (const item of iterable) {
        emitter.emitOne(mapFn(item));
      }
    });
  }
  map(mapFn) {
    return _AsyncIterableObject.map(this, mapFn);
  }
  static filter(iterable, filterFn) {
    return new _AsyncIterableObject(async (emitter) => {
      for await (const item of iterable) {
        if (filterFn(item)) {
          emitter.emitOne(item);
        }
      }
    });
  }
  filter(filterFn) {
    return _AsyncIterableObject.filter(this, filterFn);
  }
  static coalesce(iterable) {
    return _AsyncIterableObject.filter(iterable, (item) => !!item);
  }
  coalesce() {
    return _AsyncIterableObject.coalesce(this);
  }
  static async toPromise(iterable) {
    const result = [];
    for await (const item of iterable) {
      result.push(item);
    }
    return result;
  }
  toPromise() {
    return _AsyncIterableObject.toPromise(this);
  }
  /**
   * The value will be appended at the end.
   *
   * **NOTE** If `resolve()` or `reject()` have already been called, this method has no effect.
   */
  emitOne(value) {
    if (this._state !== 0 /* Initial */) {
      return;
    }
    this._results.push(value);
    this._onStateChanged.fire();
  }
  /**
   * The values will be appended at the end.
   *
   * **NOTE** If `resolve()` or `reject()` have already been called, this method has no effect.
   */
  emitMany(values) {
    if (this._state !== 0 /* Initial */) {
      return;
    }
    this._results = this._results.concat(values);
    this._onStateChanged.fire();
  }
  /**
   * Calling `resolve()` will mark the result array as complete.
   *
   * **NOTE** `resolve()` must be called, otherwise all consumers of this iterable will hang indefinitely, similar to a non-resolved promise.
   * **NOTE** If `resolve()` or `reject()` have already been called, this method has no effect.
   */
  resolve() {
    if (this._state !== 0 /* Initial */) {
      return;
    }
    this._state = 1 /* DoneOK */;
    this._onStateChanged.fire();
  }
  /**
   * Writing an error will permanently invalidate this iterable.
   * The current users will receive an error thrown, as will all future users.
   *
   * **NOTE** If `resolve()` or `reject()` have already been called, this method has no effect.
   */
  reject(error) {
    if (this._state !== 0 /* Initial */) {
      return;
    }
    this._state = 2 /* DoneError */;
    this._error = error;
    this._onStateChanged.fire();
  }
};
_AsyncIterableObject.EMPTY = _AsyncIterableObject.fromArray([]);
var AsyncIterableObject = _AsyncIterableObject;

// node_modules/@xterm/xterm/src/vs/base/common/strings.ts
function isHighSurrogate(charCode) {
  return 55296 <= charCode && charCode <= 56319;
}
function isLowSurrogate(charCode) {
  return 56320 <= charCode && charCode <= 57343;
}
function computeCodePoint(highSurrogate, lowSurrogate) {
  return (highSurrogate - 55296 << 10) + (lowSurrogate - 56320) + 65536;
}

// node_modules/@xterm/xterm/src/vs/base/common/hash.ts
function hash(obj) {
  return doHash(obj, 0);
}
function doHash(obj, hashVal) {
  switch (typeof obj) {
    case "object":
      if (obj === null) {
        return numberHash(349, hashVal);
      } else if (Array.isArray(obj)) {
        return arrayHash(obj, hashVal);
      }
      return objectHash(obj, hashVal);
    case "string":
      return stringHash(obj, hashVal);
    case "boolean":
      return booleanHash(obj, hashVal);
    case "number":
      return numberHash(obj, hashVal);
    case "undefined":
      return numberHash(937, hashVal);
    default:
      return numberHash(617, hashVal);
  }
}
function numberHash(val, initialHashVal) {
  return (initialHashVal << 5) - initialHashVal + val | 0;
}
function booleanHash(b, initialHashVal) {
  return numberHash(b ? 433 : 863, initialHashVal);
}
function stringHash(s, hashVal) {
  hashVal = numberHash(149417, hashVal);
  for (let i = 0, length = s.length; i < length; i++) {
    hashVal = numberHash(s.charCodeAt(i), hashVal);
  }
  return hashVal;
}
function arrayHash(arr, initialHashVal) {
  initialHashVal = numberHash(104579, initialHashVal);
  return arr.reduce((hashVal, item) => doHash(item, hashVal), initialHashVal);
}
function objectHash(obj, initialHashVal) {
  initialHashVal = numberHash(181387, initialHashVal);
  return Object.keys(obj).sort().reduce((hashVal, key) => {
    hashVal = stringHash(key, hashVal);
    return doHash(obj[key], hashVal);
  }, initialHashVal);
}
function leftRotate(value, bits, totalBits = 32) {
  const delta = totalBits - bits;
  const mask = ~((1 << delta) - 1);
  return (value << bits | (mask & value) >>> delta) >>> 0;
}
function fill(dest, index = 0, count = dest.byteLength, value = 0) {
  for (let i = 0; i < count; i++) {
    dest[index + i] = value;
  }
}
function leftPad(value, length, char = "0") {
  while (value.length < length) {
    value = char + value;
  }
  return value;
}
function toHexString(bufferOrValue, bitsize = 32) {
  if (bufferOrValue instanceof ArrayBuffer) {
    return Array.from(new Uint8Array(bufferOrValue)).map((b) => b.toString(16).padStart(2, "0")).join("");
  }
  return leftPad((bufferOrValue >>> 0).toString(16), bitsize / 4);
}
var _StringSHA1 = class _StringSHA1 {
  constructor() {
    // 80 * 4 = 320
    this._h0 = 1732584193;
    this._h1 = 4023233417;
    this._h2 = 2562383102;
    this._h3 = 271733878;
    this._h4 = 3285377520;
    this._buff = new Uint8Array(
      64 /* BLOCK_SIZE */ + 3
      /* to fit any utf-8 */
    );
    this._buffDV = new DataView(this._buff.buffer);
    this._buffLen = 0;
    this._totalLen = 0;
    this._leftoverHighSurrogate = 0;
    this._finished = false;
  }
  update(str) {
    const strLen = str.length;
    if (strLen === 0) {
      return;
    }
    const buff = this._buff;
    let buffLen = this._buffLen;
    let leftoverHighSurrogate = this._leftoverHighSurrogate;
    let charCode;
    let offset;
    if (leftoverHighSurrogate !== 0) {
      charCode = leftoverHighSurrogate;
      offset = -1;
      leftoverHighSurrogate = 0;
    } else {
      charCode = str.charCodeAt(0);
      offset = 0;
    }
    while (true) {
      let codePoint = charCode;
      if (isHighSurrogate(charCode)) {
        if (offset + 1 < strLen) {
          const nextCharCode = str.charCodeAt(offset + 1);
          if (isLowSurrogate(nextCharCode)) {
            offset++;
            codePoint = computeCodePoint(charCode, nextCharCode);
          } else {
            codePoint = 65533 /* UNICODE_REPLACEMENT */;
          }
        } else {
          leftoverHighSurrogate = charCode;
          break;
        }
      } else if (isLowSurrogate(charCode)) {
        codePoint = 65533 /* UNICODE_REPLACEMENT */;
      }
      buffLen = this._push(buff, buffLen, codePoint);
      offset++;
      if (offset < strLen) {
        charCode = str.charCodeAt(offset);
      } else {
        break;
      }
    }
    this._buffLen = buffLen;
    this._leftoverHighSurrogate = leftoverHighSurrogate;
  }
  _push(buff, buffLen, codePoint) {
    if (codePoint < 128) {
      buff[buffLen++] = codePoint;
    } else if (codePoint < 2048) {
      buff[buffLen++] = 192 | (codePoint & 1984) >>> 6;
      buff[buffLen++] = 128 | (codePoint & 63) >>> 0;
    } else if (codePoint < 65536) {
      buff[buffLen++] = 224 | (codePoint & 61440) >>> 12;
      buff[buffLen++] = 128 | (codePoint & 4032) >>> 6;
      buff[buffLen++] = 128 | (codePoint & 63) >>> 0;
    } else {
      buff[buffLen++] = 240 | (codePoint & 1835008) >>> 18;
      buff[buffLen++] = 128 | (codePoint & 258048) >>> 12;
      buff[buffLen++] = 128 | (codePoint & 4032) >>> 6;
      buff[buffLen++] = 128 | (codePoint & 63) >>> 0;
    }
    if (buffLen >= 64 /* BLOCK_SIZE */) {
      this._step();
      buffLen -= 64 /* BLOCK_SIZE */;
      this._totalLen += 64 /* BLOCK_SIZE */;
      buff[0] = buff[64 /* BLOCK_SIZE */ + 0];
      buff[1] = buff[64 /* BLOCK_SIZE */ + 1];
      buff[2] = buff[64 /* BLOCK_SIZE */ + 2];
    }
    return buffLen;
  }
  digest() {
    if (!this._finished) {
      this._finished = true;
      if (this._leftoverHighSurrogate) {
        this._leftoverHighSurrogate = 0;
        this._buffLen = this._push(this._buff, this._buffLen, 65533 /* UNICODE_REPLACEMENT */);
      }
      this._totalLen += this._buffLen;
      this._wrapUp();
    }
    return toHexString(this._h0) + toHexString(this._h1) + toHexString(this._h2) + toHexString(this._h3) + toHexString(this._h4);
  }
  _wrapUp() {
    this._buff[this._buffLen++] = 128;
    fill(this._buff, this._buffLen);
    if (this._buffLen > 56) {
      this._step();
      fill(this._buff);
    }
    const ml = 8 * this._totalLen;
    this._buffDV.setUint32(56, Math.floor(ml / 4294967296), false);
    this._buffDV.setUint32(60, ml % 4294967296, false);
    this._step();
  }
  _step() {
    const bigBlock32 = _StringSHA1._bigBlock32;
    const data = this._buffDV;
    for (let j = 0; j < 64; j += 4) {
      bigBlock32.setUint32(j, data.getUint32(j, false), false);
    }
    for (let j = 64; j < 320; j += 4) {
      bigBlock32.setUint32(j, leftRotate(bigBlock32.getUint32(j - 12, false) ^ bigBlock32.getUint32(j - 32, false) ^ bigBlock32.getUint32(j - 56, false) ^ bigBlock32.getUint32(j - 64, false), 1), false);
    }
    let a = this._h0;
    let b = this._h1;
    let c = this._h2;
    let d = this._h3;
    let e = this._h4;
    let f, k;
    let temp;
    for (let j = 0; j < 80; j++) {
      if (j < 20) {
        f = b & c | ~b & d;
        k = 1518500249;
      } else if (j < 40) {
        f = b ^ c ^ d;
        k = 1859775393;
      } else if (j < 60) {
        f = b & c | b & d | c & d;
        k = 2400959708;
      } else {
        f = b ^ c ^ d;
        k = 3395469782;
      }
      temp = leftRotate(a, 5) + f + e + k + bigBlock32.getUint32(j * 4, false) & 4294967295;
      e = d;
      d = c;
      c = leftRotate(b, 30);
      b = a;
      a = temp;
    }
    this._h0 = this._h0 + a & 4294967295;
    this._h1 = this._h1 + b & 4294967295;
    this._h2 = this._h2 + c & 4294967295;
    this._h3 = this._h3 + d & 4294967295;
    this._h4 = this._h4 + e & 4294967295;
  }
};
_StringSHA1._bigBlock32 = new DataView(new ArrayBuffer(320));
var StringSHA1 = _StringSHA1;

// node_modules/@xterm/xterm/src/vs/base/browser/dom.ts
var {
  registerWindow,
  getWindow,
  getDocument,
  getWindows,
  getWindowsCount,
  getWindowId,
  getWindowById,
  hasWindow,
  onDidRegisterWindow,
  onWillUnregisterWindow,
  onDidUnregisterWindow
} = function() {
  const windows = /* @__PURE__ */ new Map();
  ensureCodeWindow(mainWindow, 1);
  const mainWindowRegistration = { window: mainWindow, disposables: new DisposableStore() };
  windows.set(mainWindow.vscodeWindowId, mainWindowRegistration);
  const onDidRegisterWindow2 = new Emitter();
  const onDidUnregisterWindow2 = new Emitter();
  const onWillUnregisterWindow2 = new Emitter();
  function getWindowById2(windowId, fallbackToMain) {
    const window2 = typeof windowId === "number" ? windows.get(windowId) : void 0;
    return window2 ?? (fallbackToMain ? mainWindowRegistration : void 0);
  }
  return {
    onDidRegisterWindow: onDidRegisterWindow2.event,
    onWillUnregisterWindow: onWillUnregisterWindow2.event,
    onDidUnregisterWindow: onDidUnregisterWindow2.event,
    registerWindow(window2) {
      if (windows.has(window2.vscodeWindowId)) {
        return Disposable.None;
      }
      const disposables = new DisposableStore();
      const registeredWindow = {
        window: window2,
        disposables: disposables.add(new DisposableStore())
      };
      windows.set(window2.vscodeWindowId, registeredWindow);
      disposables.add(toDisposable(() => {
        windows.delete(window2.vscodeWindowId);
        onDidUnregisterWindow2.fire(window2);
      }));
      disposables.add(addDisposableListener(window2, EventType.BEFORE_UNLOAD, () => {
        onWillUnregisterWindow2.fire(window2);
      }));
      onDidRegisterWindow2.fire(registeredWindow);
      return disposables;
    },
    getWindows() {
      return windows.values();
    },
    getWindowsCount() {
      return windows.size;
    },
    getWindowId(targetWindow) {
      return targetWindow.vscodeWindowId;
    },
    hasWindow(windowId) {
      return windows.has(windowId);
    },
    getWindowById: getWindowById2,
    getWindow(e) {
      const candidateNode = e;
      if (candidateNode?.ownerDocument?.defaultView) {
        return candidateNode.ownerDocument.defaultView.window;
      }
      const candidateEvent = e;
      if (candidateEvent?.view) {
        return candidateEvent.view.window;
      }
      return mainWindow;
    },
    getDocument(e) {
      const candidateNode = e;
      return getWindow(candidateNode).document;
    }
  };
}();
var DomListener = class {
  constructor(node, type, handler, options) {
    this._node = node;
    this._type = type;
    this._handler = handler;
    this._options = options || false;
    this._node.addEventListener(this._type, this._handler, this._options);
  }
  dispose() {
    if (!this._handler) {
      return;
    }
    this._node.removeEventListener(this._type, this._handler, this._options);
    this._node = null;
    this._handler = null;
  }
};
function addDisposableListener(node, type, handler, useCaptureOrOptions) {
  return new DomListener(node, type, handler, useCaptureOrOptions);
}
var runAtThisOrScheduleAtNextAnimationFrame;
var scheduleAtNextAnimationFrame;
var AnimationFrameQueueItem = class {
  constructor(runner, priority = 0) {
    this._runner = runner;
    this.priority = priority;
    this._canceled = false;
  }
  dispose() {
    this._canceled = true;
  }
  execute() {
    if (this._canceled) {
      return;
    }
    try {
      this._runner();
    } catch (e) {
      onUnexpectedError(e);
    }
  }
  // Sort by priority (largest to lowest)
  static sort(a, b) {
    return b.priority - a.priority;
  }
};
(function() {
  const NEXT_QUEUE = /* @__PURE__ */ new Map();
  const CURRENT_QUEUE = /* @__PURE__ */ new Map();
  const animFrameRequested = /* @__PURE__ */ new Map();
  const inAnimationFrameRunner = /* @__PURE__ */ new Map();
  const animationFrameRunner = (targetWindowId) => {
    animFrameRequested.set(targetWindowId, false);
    const currentQueue = NEXT_QUEUE.get(targetWindowId) ?? [];
    CURRENT_QUEUE.set(targetWindowId, currentQueue);
    NEXT_QUEUE.set(targetWindowId, []);
    inAnimationFrameRunner.set(targetWindowId, true);
    while (currentQueue.length > 0) {
      currentQueue.sort(AnimationFrameQueueItem.sort);
      const top = currentQueue.shift();
      top.execute();
    }
    inAnimationFrameRunner.set(targetWindowId, false);
  };
  scheduleAtNextAnimationFrame = (targetWindow, runner, priority = 0) => {
    const targetWindowId = getWindowId(targetWindow);
    const item = new AnimationFrameQueueItem(runner, priority);
    let nextQueue = NEXT_QUEUE.get(targetWindowId);
    if (!nextQueue) {
      nextQueue = [];
      NEXT_QUEUE.set(targetWindowId, nextQueue);
    }
    nextQueue.push(item);
    if (!animFrameRequested.get(targetWindowId)) {
      animFrameRequested.set(targetWindowId, true);
      targetWindow.requestAnimationFrame(() => animationFrameRunner(targetWindowId));
    }
    return item;
  };
  runAtThisOrScheduleAtNextAnimationFrame = (targetWindow, runner, priority) => {
    const targetWindowId = getWindowId(targetWindow);
    if (inAnimationFrameRunner.get(targetWindowId)) {
      const item = new AnimationFrameQueueItem(runner, priority);
      let currentQueue = CURRENT_QUEUE.get(targetWindowId);
      if (!currentQueue) {
        currentQueue = [];
        CURRENT_QUEUE.set(targetWindowId, currentQueue);
      }
      currentQueue.push(item);
      return item;
    } else {
      return scheduleAtNextAnimationFrame(targetWindow, runner, priority);
    }
  };
})();
var _Dimension = class _Dimension {
  constructor(width, height) {
    this.width = width;
    this.height = height;
  }
  with(width = this.width, height = this.height) {
    if (width !== this.width || height !== this.height) {
      return new _Dimension(width, height);
    } else {
      return this;
    }
  }
  static is(obj) {
    return typeof obj === "object" && typeof obj.height === "number" && typeof obj.width === "number";
  }
  static lift(obj) {
    if (obj instanceof _Dimension) {
      return obj;
    } else {
      return new _Dimension(obj.width, obj.height);
    }
  }
  static equals(a, b) {
    if (a === b) {
      return true;
    }
    if (!a || !b) {
      return false;
    }
    return a.width === b.width && a.height === b.height;
  }
};
_Dimension.None = new _Dimension(0, 0);
var Dimension = _Dimension;
var sharedMutationObserver = new class {
  constructor() {
    this.mutationObservers = /* @__PURE__ */ new Map();
  }
  observe(target, disposables, options) {
    let mutationObserversPerTarget = this.mutationObservers.get(target);
    if (!mutationObserversPerTarget) {
      mutationObserversPerTarget = /* @__PURE__ */ new Map();
      this.mutationObservers.set(target, mutationObserversPerTarget);
    }
    const optionsHash = hash(options);
    let mutationObserverPerOptions = mutationObserversPerTarget.get(optionsHash);
    if (!mutationObserverPerOptions) {
      const onDidMutate = new Emitter();
      const observer = new MutationObserver((mutations) => onDidMutate.fire(mutations));
      observer.observe(target, options);
      const resolvedMutationObserverPerOptions = mutationObserverPerOptions = {
        users: 1,
        observer,
        onDidMutate: onDidMutate.event
      };
      disposables.add(toDisposable(() => {
        resolvedMutationObserverPerOptions.users -= 1;
        if (resolvedMutationObserverPerOptions.users === 0) {
          onDidMutate.dispose();
          observer.disconnect();
          mutationObserversPerTarget?.delete(optionsHash);
          if (mutationObserversPerTarget?.size === 0) {
            this.mutationObservers.delete(target);
          }
        }
      }));
      mutationObserversPerTarget.set(optionsHash, mutationObserverPerOptions);
    } else {
      mutationObserverPerOptions.users += 1;
    }
    return mutationObserverPerOptions.onDidMutate;
  }
}();
var EventType = {
  // Mouse
  CLICK: "click",
  AUXCLICK: "auxclick",
  DBLCLICK: "dblclick",
  MOUSE_UP: "mouseup",
  MOUSE_DOWN: "mousedown",
  MOUSE_OVER: "mouseover",
  MOUSE_MOVE: "mousemove",
  MOUSE_OUT: "mouseout",
  MOUSE_ENTER: "mouseenter",
  MOUSE_LEAVE: "mouseleave",
  MOUSE_WHEEL: "wheel",
  POINTER_UP: "pointerup",
  POINTER_DOWN: "pointerdown",
  POINTER_MOVE: "pointermove",
  POINTER_LEAVE: "pointerleave",
  CONTEXT_MENU: "contextmenu",
  WHEEL: "wheel",
  // Keyboard
  KEY_DOWN: "keydown",
  KEY_PRESS: "keypress",
  KEY_UP: "keyup",
  // HTML Document
  LOAD: "load",
  BEFORE_UNLOAD: "beforeunload",
  UNLOAD: "unload",
  PAGE_SHOW: "pageshow",
  PAGE_HIDE: "pagehide",
  PASTE: "paste",
  ABORT: "abort",
  ERROR: "error",
  RESIZE: "resize",
  SCROLL: "scroll",
  FULLSCREEN_CHANGE: "fullscreenchange",
  WK_FULLSCREEN_CHANGE: "webkitfullscreenchange",
  // Form
  SELECT: "select",
  CHANGE: "change",
  SUBMIT: "submit",
  RESET: "reset",
  FOCUS: "focus",
  FOCUS_IN: "focusin",
  FOCUS_OUT: "focusout",
  BLUR: "blur",
  INPUT: "input",
  // Local Storage
  STORAGE: "storage",
  // Drag
  DRAG_START: "dragstart",
  DRAG: "drag",
  DRAG_ENTER: "dragenter",
  DRAG_LEAVE: "dragleave",
  DRAG_OVER: "dragover",
  DROP: "drop",
  DRAG_END: "dragend",
  // Animation
  ANIMATION_START: isWebKit ? "webkitAnimationStart" : "animationstart",
  ANIMATION_END: isWebKit ? "webkitAnimationEnd" : "animationend",
  ANIMATION_ITERATION: isWebKit ? "webkitAnimationIteration" : "animationiteration"
};
var SELECTOR_REGEX = /([\w\-]+)?(#([\w\-]+))?((\.([\w\-]+))*)/;
function _$(namespace, description, attrs, ...children) {
  const match = SELECTOR_REGEX.exec(description);
  if (!match) {
    throw new Error("Bad use of emmet");
  }
  const tagName = match[1] || "div";
  let result;
  if (namespace !== "http://www.w3.org/1999/xhtml" /* HTML */) {
    result = document.createElementNS(namespace, tagName);
  } else {
    result = document.createElement(tagName);
  }
  if (match[3]) {
    result.id = match[3];
  }
  if (match[4]) {
    result.className = match[4].replace(/\./g, " ").trim();
  }
  if (attrs) {
    Object.entries(attrs).forEach(([name, value]) => {
      if (typeof value === "undefined") {
        return;
      }
      if (/^on\w+$/.test(name)) {
        result[name] = value;
      } else if (name === "selected") {
        if (value) {
          result.setAttribute(name, "true");
        }
      } else {
        result.setAttribute(name, value);
      }
    });
  }
  result.append(...children);
  return result;
}
function $(description, attrs, ...children) {
  return _$("http://www.w3.org/1999/xhtml" /* HTML */, description, attrs, ...children);
}
$.SVG = function(description, attrs, ...children) {
  return _$("http://www.w3.org/2000/svg" /* SVG */, description, attrs, ...children);
};

// vendor/xterm-addon-webgl/src/WebglRenderer.ts
var WebglRenderer = class extends Disposable {
  constructor(_terminal, _characterJoinerService, _charSizeService, _coreBrowserService, _coreService, _decorationService, _optionsService, _themeService, preserveDrawingBuffer) {
    super();
    this._terminal = _terminal;
    this._characterJoinerService = _characterJoinerService;
    this._charSizeService = _charSizeService;
    this._coreBrowserService = _coreBrowserService;
    this._coreService = _coreService;
    this._decorationService = _decorationService;
    this._optionsService = _optionsService;
    this._themeService = _themeService;
    this._cursorBlinkStateManager = new MutableDisposable();
    this._charAtlasDisposable = this._register(new MutableDisposable());
    this._observerDisposable = this._register(new MutableDisposable());
    this._model = new RenderModel();
    this._workCell = new CellData();
    this._workCell2 = new CellData();
    this._rectangleRenderer = this._register(new MutableDisposable());
    this._glyphRenderer = this._register(new MutableDisposable());
    this._onChangeTextureAtlas = this._register(new Emitter());
    this.onChangeTextureAtlas = this._onChangeTextureAtlas.event;
    this._onAddTextureAtlasCanvas = this._register(new Emitter());
    this.onAddTextureAtlasCanvas = this._onAddTextureAtlasCanvas.event;
    this._onRemoveTextureAtlasCanvas = this._register(new Emitter());
    this.onRemoveTextureAtlasCanvas = this._onRemoveTextureAtlasCanvas.event;
    this._onRequestRedraw = this._register(new Emitter());
    this.onRequestRedraw = this._onRequestRedraw.event;
    this._onContextLoss = this._register(new Emitter());
    this.onContextLoss = this._onContextLoss.event;
    this._canvas = this._coreBrowserService.mainDocument.createElement("canvas");
    const contextAttributes = {
      antialias: false,
      depth: false,
      preserveDrawingBuffer
    };
    this._gl = this._canvas.getContext("webgl2", contextAttributes);
    if (!this._gl) {
      throw new Error("WebGL2 not supported " + this._gl);
    }
    this._register(this._themeService.onChangeColors(() => this._handleColorChange()));
    this._cellColorResolver = new CellColorResolver(this._terminal, this._optionsService, this._model.selection, this._decorationService, this._coreBrowserService, this._themeService);
    this._core = this._terminal._core;
    this._renderLayers = [
      new LinkRenderLayer(this._core.screenElement, 2, this._terminal, this._core.linkifier, this._coreBrowserService, _optionsService, this._themeService)
    ];
    this.dimensions = createRenderDimensions();
    this._devicePixelRatio = this._coreBrowserService.dpr;
    this._updateDimensions();
    this._updateCursorBlink();
    this._register(_optionsService.onOptionChange(() => this._handleOptionsChanged()));
    this._deviceMaxTextureSize = this._gl.getParameter(this._gl.MAX_TEXTURE_SIZE);
    this._register(addDisposableListener(this._canvas, "webglcontextlost", (e) => {
      console.log("webglcontextlost event received");
      e.preventDefault();
      this._contextRestorationTimeout = setTimeout(
        () => {
          this._contextRestorationTimeout = void 0;
          console.warn("webgl context not restored; firing onContextLoss");
          this._onContextLoss.fire(e);
        },
        3e3
        /* ms */
      );
    }));
    this._register(addDisposableListener(this._canvas, "webglcontextrestored", (e) => {
      console.warn("webglcontextrestored event received");
      clearTimeout(this._contextRestorationTimeout);
      this._contextRestorationTimeout = void 0;
      removeTerminalFromCache(this._terminal);
      this._initializeWebGLState();
      this._requestRedrawViewport();
    }));
    this._observerDisposable.value = observeDevicePixelDimensions(this._canvas, this._coreBrowserService.window, (w, h) => this._setCanvasDevicePixelDimensions(w, h));
    this._register(this._coreBrowserService.onWindowChange((w) => {
      this._observerDisposable.value = observeDevicePixelDimensions(this._canvas, w, (w2, h) => this._setCanvasDevicePixelDimensions(w2, h));
    }));
    this._core.screenElement.appendChild(this._canvas);
    [this._rectangleRenderer.value, this._glyphRenderer.value] = this._initializeWebGLState();
    this._isAttached = this._core.screenElement.isConnected;
    this._register(toDisposable(() => {
      for (const l of this._renderLayers) {
        l.dispose();
      }
      this._canvas.parentElement?.removeChild(this._canvas);
      removeTerminalFromCache(this._terminal);
    }));
  }
  get textureAtlas() {
    return this._charAtlas?.pages[0].canvas;
  }
  _handleColorChange() {
    this._refreshCharAtlas();
    this._clearModel(true);
  }
  handleDevicePixelRatioChange() {
    if (this._devicePixelRatio !== this._coreBrowserService.dpr) {
      this._devicePixelRatio = this._coreBrowserService.dpr;
      this.handleResize(this._terminal.cols, this._terminal.rows);
    }
  }
  handleResize(cols, rows) {
    this._updateDimensions();
    this._model.resize(this._terminal.cols, this._terminal.rows);
    for (const l of this._renderLayers) {
      l.resize(this._terminal, this.dimensions);
    }
    this._canvas.width = this.dimensions.device.canvas.width;
    this._canvas.height = this.dimensions.device.canvas.height;
    this._canvas.style.width = `${this.dimensions.css.canvas.width}px`;
    this._canvas.style.height = `${this.dimensions.css.canvas.height}px`;
    this._core.screenElement.style.width = `${this.dimensions.css.canvas.width}px`;
    this._core.screenElement.style.height = `${this.dimensions.css.canvas.height}px`;
    this._rectangleRenderer.value?.setDimensions(this.dimensions);
    this._rectangleRenderer.value?.handleResize();
    this._glyphRenderer.value?.setDimensions(this.dimensions);
    this._glyphRenderer.value?.handleResize();
    this._refreshCharAtlas();
    this._clearModel(false);
  }
  handleCharSizeChanged() {
    this.handleResize(this._terminal.cols, this._terminal.rows);
  }
  handleBlur() {
    for (const l of this._renderLayers) {
      l.handleBlur(this._terminal);
    }
    this._cursorBlinkStateManager.value?.pause();
    this._requestRedrawViewport();
  }
  handleFocus() {
    for (const l of this._renderLayers) {
      l.handleFocus(this._terminal);
    }
    this._cursorBlinkStateManager.value?.resume();
    this._requestRedrawViewport();
  }
  handleSelectionChanged(start, end, columnSelectMode) {
    for (const l of this._renderLayers) {
      l.handleSelectionChanged(this._terminal, start, end, columnSelectMode);
    }
    this._model.selection.update(this._core, start, end, columnSelectMode);
    this._requestRedrawViewport();
  }
  handleCursorMove() {
    for (const l of this._renderLayers) {
      l.handleCursorMove(this._terminal);
    }
    this._cursorBlinkStateManager.value?.restartBlinkAnimation();
  }
  _handleOptionsChanged() {
    this._updateDimensions();
    this._refreshCharAtlas();
    this._updateCursorBlink();
  }
  /**
   * Initializes members dependent on WebGL context state.
   */
  _initializeWebGLState() {
    this._rectangleRenderer.value = new RectangleRenderer(this._terminal, this._gl, this.dimensions, this._themeService);
    this._glyphRenderer.value = new GlyphRenderer(this._terminal, this._gl, this.dimensions, this._optionsService);
    this.handleCharSizeChanged();
    return [this._rectangleRenderer.value, this._glyphRenderer.value];
  }
  /**
   * Refreshes the char atlas, aquiring a new one if necessary.
   */
  _refreshCharAtlas() {
    if (this.dimensions.device.char.width <= 0 && this.dimensions.device.char.height <= 0) {
      this._isAttached = false;
      return;
    }
    const atlas = acquireTextureAtlas(
      this._terminal,
      this._optionsService.rawOptions,
      this._themeService.colors,
      this.dimensions.device.cell.width,
      this.dimensions.device.cell.height,
      this.dimensions.device.char.width,
      this.dimensions.device.char.height,
      this._coreBrowserService.dpr,
      this._deviceMaxTextureSize
    );
    if (this._charAtlas !== atlas) {
      this._onChangeTextureAtlas.fire(atlas.pages[0].canvas);
      this._charAtlasDisposable.value = combinedDisposable(
        Event.forward(atlas.onAddTextureAtlasCanvas, this._onAddTextureAtlasCanvas),
        Event.forward(atlas.onRemoveTextureAtlasCanvas, this._onRemoveTextureAtlasCanvas)
      );
    }
    this._charAtlas = atlas;
    this._charAtlas.warmUp();
    this._glyphRenderer.value?.setAtlas(this._charAtlas);
  }
  /**
   * Clear the model.
   * @param clearGlyphRenderer Whether to also clear the glyph renderer. This
   * should be true generally to make sure it is in the same state as the model.
   */
  _clearModel(clearGlyphRenderer) {
    this._model.clear();
    if (clearGlyphRenderer) {
      this._glyphRenderer.value?.clear();
    }
  }
  clearTextureAtlas() {
    this._charAtlas?.clearTexture();
    this._clearModel(true);
    this._requestRedrawViewport();
  }
  clear() {
    this._clearModel(true);
    for (const l of this._renderLayers) {
      l.reset(this._terminal);
    }
    this._cursorBlinkStateManager.value?.restartBlinkAnimation();
    this._updateCursorBlink();
  }
  renderRows(start, end) {
    if (!this._isAttached) {
      if (this._core.screenElement?.isConnected && this._charSizeService.width && this._charSizeService.height) {
        this._updateDimensions();
        this._refreshCharAtlas();
        this._isAttached = true;
      } else {
        return;
      }
    }
    for (const l of this._renderLayers) {
      l.handleGridChanged(this._terminal, start, end);
    }
    if (!this._glyphRenderer.value || !this._rectangleRenderer.value) {
      return;
    }
    if (this._glyphRenderer.value.beginFrame()) {
      this._clearModel(true);
      this._updateModel(0, this._terminal.rows - 1);
    } else {
      this._updateModel(start, end);
    }
    let merged = false;
    let mergeRetries = 0;
    while (this._charAtlas && this._glyphRenderer.value.beginFrame() && mergeRetries++ < 32 /* MERGE_RETRY_LIMIT */) {
      merged = true;
      this._clearModel(true);
      this._updateModel(0, this._terminal.rows - 1);
    }
    if (merged) {
      this._glyphRenderer.value.invalidateAtlasTextures();
    }
    this._rectangleRenderer.value.renderBackgrounds();
    this._glyphRenderer.value.render(this._model);
    if (!this._cursorBlinkStateManager.value || this._cursorBlinkStateManager.value.isCursorVisible) {
      this._rectangleRenderer.value.renderCursor();
    }
  }
  _updateCursorBlink() {
    if (this._coreService.decPrivateModes.cursorBlink ?? this._terminal.options.cursorBlink) {
      this._cursorBlinkStateManager.value = new CursorBlinkStateManager(() => {
        this._requestRedrawCursor();
      }, this._coreBrowserService);
    } else {
      this._cursorBlinkStateManager.clear();
    }
    this._requestRedrawCursor();
  }
  _updateModel(start, end) {
    const terminal = this._core;
    let cell = this._workCell;
    let lastBg;
    let y;
    let row;
    let line;
    let joinedRanges;
    let isJoined;
    let skipJoinedCheckUntilX = 0;
    let isValidJoinRange = true;
    let lastCharX;
    let range;
    let isCursorRow;
    let chars;
    let code;
    let width;
    let i;
    let x;
    let j;
    start = clamp2(start, terminal.rows - 1, 0);
    end = clamp2(end, terminal.rows - 1, 0);
    const cursorStyle = this._coreService.decPrivateModes.cursorStyle ?? terminal.options.cursorStyle ?? "block";
    const cursorY = this._terminal.buffer.active.baseY + this._terminal.buffer.active.cursorY;
    const viewportRelativeCursorY = cursorY - terminal.buffer.ydisp;
    const cursorX = Math.min(this._terminal.buffer.active.cursorX, terminal.cols - 1);
    let lastCursorX = -1;
    const isCursorVisible = this._coreService.isCursorInitialized && !this._coreService.isCursorHidden && (!this._cursorBlinkStateManager.value || this._cursorBlinkStateManager.value.isCursorVisible);
    this._model.cursor = void 0;
    let modelUpdated = false;
    for (y = start; y <= end; y++) {
      row = y + terminal.buffer.ydisp;
      line = terminal.buffer.lines.get(row);
      this._model.lineLengths[y] = 0;
      isCursorRow = cursorY === row;
      skipJoinedCheckUntilX = 0;
      joinedRanges = this._characterJoinerService.getJoinedCharacters(row);
      for (x = 0; x < terminal.cols; x++) {
        lastBg = this._cellColorResolver.result.bg;
        line.loadCell(x, cell);
        if (x === 0) {
          lastBg = this._cellColorResolver.result.bg;
        }
        isJoined = false;
        isValidJoinRange = x >= skipJoinedCheckUntilX;
        lastCharX = x;
        if (joinedRanges.length > 0 && x === joinedRanges[0][0] && isValidJoinRange) {
          range = joinedRanges.shift();
          const firstSelectionState = this._model.selection.isCellSelected(this._terminal, range[0], row);
          for (i = range[0] + 1; i < range[1]; i++) {
            isValidJoinRange &&= firstSelectionState === this._model.selection.isCellSelected(this._terminal, i, row);
          }
          isValidJoinRange &&= !isCursorRow || cursorX < range[0] || cursorX >= range[1];
          if (!isValidJoinRange) {
            skipJoinedCheckUntilX = range[1];
          } else {
            isJoined = true;
            cell = new JoinedCellData(
              cell,
              line.translateToString(true, range[0], range[1]),
              range[1] - range[0]
            );
            lastCharX = range[1] - 1;
          }
        }
        chars = cell.getChars();
        code = cell.getCode();
        i = (y * terminal.cols + x) * RENDER_MODEL_INDICIES_PER_CELL;
        this._cellColorResolver.resolve(cell, x, row, this.dimensions.device.cell.width);
        if (isCursorVisible && row === cursorY) {
          if (x === cursorX) {
            this._model.cursor = {
              x: cursorX,
              y: viewportRelativeCursorY,
              width: cell.getWidth(),
              style: this._coreBrowserService.isFocused ? cursorStyle : terminal.options.cursorInactiveStyle,
              cursorWidth: terminal.options.cursorWidth,
              dpr: this._devicePixelRatio
            };
            lastCursorX = cursorX + cell.getWidth() - 1;
          }
          if (x >= cursorX && x <= lastCursorX && (this._coreBrowserService.isFocused && cursorStyle === "block" || this._coreBrowserService.isFocused === false && terminal.options.cursorInactiveStyle === "block")) {
            this._cellColorResolver.result.fg = 50331648 /* CM_RGB */ | this._themeService.colors.cursorAccent.rgba >> 8 & 16777215 /* RGB_MASK */;
            this._cellColorResolver.result.bg = 50331648 /* CM_RGB */ | this._themeService.colors.cursor.rgba >> 8 & 16777215 /* RGB_MASK */;
          }
        }
        if (code !== NULL_CELL_CODE) {
          this._model.lineLengths[y] = x + 1;
        }
        if (this._model.cells[i] === code && this._model.cells[i + RENDER_MODEL_BG_OFFSET] === this._cellColorResolver.result.bg && this._model.cells[i + RENDER_MODEL_FG_OFFSET] === this._cellColorResolver.result.fg && this._model.cells[i + RENDER_MODEL_EXT_OFFSET] === this._cellColorResolver.result.ext) {
          continue;
        }
        modelUpdated = true;
        if (chars.length > 1) {
          code |= COMBINED_CHAR_BIT_MASK;
        }
        this._model.cells[i] = code;
        this._model.cells[i + RENDER_MODEL_BG_OFFSET] = this._cellColorResolver.result.bg;
        this._model.cells[i + RENDER_MODEL_FG_OFFSET] = this._cellColorResolver.result.fg;
        this._model.cells[i + RENDER_MODEL_EXT_OFFSET] = this._cellColorResolver.result.ext;
        width = cell.getWidth();
        this._glyphRenderer.value.updateCell(x, y, code, this._cellColorResolver.result.bg, this._cellColorResolver.result.fg, this._cellColorResolver.result.ext, chars, width, lastBg);
        if (isJoined) {
          cell = this._workCell;
          for (x++; x <= lastCharX; x++) {
            j = (y * terminal.cols + x) * RENDER_MODEL_INDICIES_PER_CELL;
            this._glyphRenderer.value.updateCell(x, y, NULL_CELL_CODE, 0, 0, 0, NULL_CELL_CHAR, 0, 0);
            this._model.cells[j] = NULL_CELL_CODE;
            this._model.cells[j + RENDER_MODEL_BG_OFFSET] = this._cellColorResolver.result.bg;
            this._model.cells[j + RENDER_MODEL_FG_OFFSET] = this._cellColorResolver.result.fg;
            this._model.cells[j + RENDER_MODEL_EXT_OFFSET] = this._cellColorResolver.result.ext;
          }
          x--;
        }
      }
    }
    if (modelUpdated) {
      this._rectangleRenderer.value.updateBackgrounds(this._model);
    }
    this._rectangleRenderer.value.updateCursor(this._model);
  }
  /**
   * Recalculates the character and canvas dimensions.
   */
  _updateDimensions() {
    if (!this._charSizeService.width || !this._charSizeService.height) {
      return;
    }
    this.dimensions.device.char.width = Math.floor(this._charSizeService.width * this._devicePixelRatio);
    this.dimensions.device.char.height = Math.ceil(this._charSizeService.height * this._devicePixelRatio);
    this.dimensions.device.cell.height = Math.floor(this.dimensions.device.char.height * this._optionsService.rawOptions.lineHeight);
    this.dimensions.device.char.top = this._optionsService.rawOptions.lineHeight === 1 ? 0 : Math.round((this.dimensions.device.cell.height - this.dimensions.device.char.height) / 2);
    this.dimensions.device.cell.width = this.dimensions.device.char.width + Math.round(this._optionsService.rawOptions.letterSpacing);
    this.dimensions.device.char.left = Math.floor(this._optionsService.rawOptions.letterSpacing / 2);
    this.dimensions.device.canvas.height = this._terminal.rows * this.dimensions.device.cell.height;
    this.dimensions.device.canvas.width = this._terminal.cols * this.dimensions.device.cell.width;
    this.dimensions.css.canvas.height = Math.round(this.dimensions.device.canvas.height / this._devicePixelRatio);
    this.dimensions.css.canvas.width = Math.round(this.dimensions.device.canvas.width / this._devicePixelRatio);
    this.dimensions.css.cell.height = this.dimensions.device.cell.height / this._devicePixelRatio;
    this.dimensions.css.cell.width = this.dimensions.device.cell.width / this._devicePixelRatio;
  }
  _setCanvasDevicePixelDimensions(width, height) {
    if (this._canvas.width === width && this._canvas.height === height) {
      return;
    }
    this._canvas.width = width;
    this._canvas.height = height;
    this._requestRedrawViewport();
  }
  _requestRedrawViewport() {
    this._onRequestRedraw.fire({ start: 0, end: this._terminal.rows - 1 });
  }
  _requestRedrawCursor() {
    const cursorY = this._terminal.buffer.active.cursorY;
    this._onRequestRedraw.fire({ start: cursorY, end: cursorY });
  }
};
var JoinedCellData = class extends AttributeData {
  constructor(firstCell, chars, width) {
    super();
    // .content carries no meaning for joined CellData, simply nullify it
    // thus we have to overload all other .content accessors
    this.content = 0;
    this.combinedData = "";
    this.fg = firstCell.fg;
    this.bg = firstCell.bg;
    this.combinedData = chars;
    this._width = width;
  }
  isCombined() {
    return 2097152 /* IS_COMBINED_MASK */;
  }
  getWidth() {
    return this._width;
  }
  getChars() {
    return this.combinedData;
  }
  getCode() {
    return 2097151;
  }
  setFromCharData(value) {
    throw new Error("not implemented");
  }
  getAsCharData() {
    return [this.fg, this.getChars(), this.getWidth(), this.getCode()];
  }
};
function clamp2(value, max, min = 0) {
  return Math.max(Math.min(value, max), min);
}

// node_modules/@xterm/xterm/src/common/services/ServiceRegistry.ts
var DI_TARGET = "di$target";
var DI_DEPENDENCIES = "di$dependencies";
var serviceRegistry = /* @__PURE__ */ new Map();
function createDecorator(id2) {
  if (serviceRegistry.has(id2)) {
    return serviceRegistry.get(id2);
  }
  const decorator = function(target, key, index) {
    if (arguments.length !== 3) {
      throw new Error("@IServiceName-decorator can only be used to decorate a parameter");
    }
    storeServiceDependency(decorator, target, index);
  };
  decorator._id = id2;
  serviceRegistry.set(id2, decorator);
  return decorator;
}
function storeServiceDependency(id2, target, index) {
  if (target[DI_TARGET] === target) {
    target[DI_DEPENDENCIES].push({ id: id2, index });
  } else {
    target[DI_DEPENDENCIES] = [{ id: id2, index }];
    target[DI_TARGET] = target;
  }
}

// node_modules/@xterm/xterm/src/common/services/Services.ts
var IBufferService = createDecorator("BufferService");
var ICoreMouseService = createDecorator("CoreMouseService");
var ICoreService = createDecorator("CoreService");
var ICharsetService = createDecorator("CharsetService");
var IInstantiationService = createDecorator("InstantiationService");
var ILogService = createDecorator("LogService");
var IOptionsService = createDecorator("OptionsService");
var IOscLinkService = createDecorator("OscLinkService");
var IUnicodeService = createDecorator("UnicodeService");
var IDecorationService = createDecorator("DecorationService");

// node_modules/@xterm/xterm/src/common/services/LogService.ts
var optionsKeyToLogLevel = {
  trace: 0 /* TRACE */,
  debug: 1 /* DEBUG */,
  info: 2 /* INFO */,
  warn: 3 /* WARN */,
  error: 4 /* ERROR */,
  off: 5 /* OFF */
};
var LOG_PREFIX = "xterm.js: ";
var LogService = class extends Disposable {
  constructor(_optionsService) {
    super();
    this._optionsService = _optionsService;
    this._logLevel = 5 /* OFF */;
    this._updateLogLevel();
    this._register(this._optionsService.onSpecificOptionChange("logLevel", () => this._updateLogLevel()));
    traceLogger = this;
  }
  get logLevel() {
    return this._logLevel;
  }
  _updateLogLevel() {
    this._logLevel = optionsKeyToLogLevel[this._optionsService.rawOptions.logLevel];
  }
  _evalLazyOptionalParams(optionalParams) {
    for (let i = 0; i < optionalParams.length; i++) {
      if (typeof optionalParams[i] === "function") {
        optionalParams[i] = optionalParams[i]();
      }
    }
  }
  _log(type, message, optionalParams) {
    this._evalLazyOptionalParams(optionalParams);
    type.call(console, (this._optionsService.options.logger ? "" : LOG_PREFIX) + message, ...optionalParams);
  }
  trace(message, ...optionalParams) {
    if (this._logLevel <= 0 /* TRACE */) {
      this._log(this._optionsService.options.logger?.trace.bind(this._optionsService.options.logger) ?? console.log, message, optionalParams);
    }
  }
  debug(message, ...optionalParams) {
    if (this._logLevel <= 1 /* DEBUG */) {
      this._log(this._optionsService.options.logger?.debug.bind(this._optionsService.options.logger) ?? console.log, message, optionalParams);
    }
  }
  info(message, ...optionalParams) {
    if (this._logLevel <= 2 /* INFO */) {
      this._log(this._optionsService.options.logger?.info.bind(this._optionsService.options.logger) ?? console.info, message, optionalParams);
    }
  }
  warn(message, ...optionalParams) {
    if (this._logLevel <= 3 /* WARN */) {
      this._log(this._optionsService.options.logger?.warn.bind(this._optionsService.options.logger) ?? console.warn, message, optionalParams);
    }
  }
  error(message, ...optionalParams) {
    if (this._logLevel <= 4 /* ERROR */) {
      this._log(this._optionsService.options.logger?.error.bind(this._optionsService.options.logger) ?? console.error, message, optionalParams);
    }
  }
};
LogService = __decorateClass([
  __decorateParam(0, IOptionsService)
], LogService);
var traceLogger;
function setTraceLogger(logger) {
  traceLogger = logger;
}

// vendor/xterm-addon-webgl/src/WebglAddon.ts
var WebglAddon = class extends Disposable {
  constructor(_preserveDrawingBuffer) {
    if (isSafari && getSafariVersion() < 16) {
      const contextAttributes = {
        antialias: false,
        depth: false,
        preserveDrawingBuffer: true
      };
      const gl = document.createElement("canvas").getContext("webgl2", contextAttributes);
      if (!gl) {
        throw new Error("Webgl2 is only supported on Safari 16 and above");
      }
    }
    super();
    this._preserveDrawingBuffer = _preserveDrawingBuffer;
    this._onChangeTextureAtlas = this._register(new Emitter());
    this.onChangeTextureAtlas = this._onChangeTextureAtlas.event;
    this._onAddTextureAtlasCanvas = this._register(new Emitter());
    this.onAddTextureAtlasCanvas = this._onAddTextureAtlasCanvas.event;
    this._onRemoveTextureAtlasCanvas = this._register(new Emitter());
    this.onRemoveTextureAtlasCanvas = this._onRemoveTextureAtlasCanvas.event;
    this._onContextLoss = this._register(new Emitter());
    this.onContextLoss = this._onContextLoss.event;
  }
  activate(terminal) {
    const core = terminal._core;
    if (!terminal.element) {
      this._register(core.onWillOpen(() => this.activate(terminal)));
      return;
    }
    this._terminal = terminal;
    const coreService = core.coreService;
    const optionsService = core.optionsService;
    const unsafeCore = core;
    const renderService = unsafeCore._renderService;
    const characterJoinerService = unsafeCore._characterJoinerService;
    const charSizeService = unsafeCore._charSizeService;
    const coreBrowserService = unsafeCore._coreBrowserService;
    const decorationService = unsafeCore._decorationService;
    const logService = unsafeCore._logService;
    const themeService = unsafeCore._themeService;
    setTraceLogger(logService);
    this._renderer = this._register(new WebglRenderer(
      terminal,
      characterJoinerService,
      charSizeService,
      coreBrowserService,
      coreService,
      decorationService,
      optionsService,
      themeService,
      this._preserveDrawingBuffer
    ));
    this._register(Event.forward(this._renderer.onContextLoss, this._onContextLoss));
    this._register(Event.forward(this._renderer.onChangeTextureAtlas, this._onChangeTextureAtlas));
    this._register(Event.forward(this._renderer.onAddTextureAtlasCanvas, this._onAddTextureAtlasCanvas));
    this._register(Event.forward(this._renderer.onRemoveTextureAtlasCanvas, this._onRemoveTextureAtlasCanvas));
    renderService.setRenderer(this._renderer);
    this._register(toDisposable(() => {
      if (this._terminal._core._store._isDisposed) {
        return;
      }
      const renderService2 = this._terminal._core._renderService;
      renderService2.setRenderer(this._terminal._core._createRenderer());
      renderService2.handleResize(terminal.cols, terminal.rows);
    }));
  }
  get textureAtlas() {
    return this._renderer?.textureAtlas;
  }
  clearTextureAtlas() {
    this._renderer?.clearTextureAtlas();
  }
};
export {
  WebglAddon
};
