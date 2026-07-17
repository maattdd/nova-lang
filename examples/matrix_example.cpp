// Matrix trait example — compiles to C++ with multiple dispatch
//
// trait Matrix[T] as M {
//     fn size(m: M) -> Int;
//     fn get(m: M, ~idx: Int) -> T;
// }
//
// struct StandardMatrix[T] {
//     elements: Vec[T];
// }
//
// impl Matrix[T] for StandardMatrix[T] {
//     fn size(m: StandardMatrix[T]) -> Int { m.elements.size() }
//     fn get(m: StandardMatrix[T], ~idx: Int) -> T { m.elements[idx] }
// }
//
// struct HotVector {
//     idx: Int;
//     size: Int;
// }
//
// impl Matrix[Int] for HotVector {
//     fn size(h: HotVector) -> Int { h.size }
//     fn get(h: HotVector, ~idx: Int) -> Int { if h.idx == idx { 1 } else { 0 } }
// }
//
// // Default — any two Matrix types
// fn inner(a: Matrix, b: Matrix) -> Int {
//     a.size() * b.size()
// }
//
// // Specialized — when second arg is HotVector
// fn inner(a: Matrix, b: HotVector) -> Int {
//     a.get(b.idx) * 2
// }
//
// fn main() -> Int {
//     let dense = StandardMatrix { elements: [1, 2, 3, 4, 5] };
//     let hot = HotVector { idx: 2, size: 10 };
//     let result = inner(dense, hot);  // 6 (specialized!)
//     print_int(result);
//     return 0
// }

// ─── Generated C++ for the above ───
#include <cstdint>
#include <iostream>
#include <vector>
#include <unordered_map>

// Trait: Matrix_vtable + fat pointer Matrix
struct Matrix_vtable { uint32_t type_id; int32_t (*size)(void*); int32_t (*get)(void*, int32_t); };
struct Matrix { void* _ptr; Matrix_vtable* _vt;
  int32_t size() { return _vt->size(_ptr); }
  int32_t get(int32_t i) { return _vt->get(_ptr, i); }
};

// StandardMatrix
struct StandardMatrix { std::vector<int32_t> elements; };
int32_t Std_size(void* s) { return ((StandardMatrix*)s)->elements.size(); }
int32_t Std_get(void* s, int32_t i) { return ((StandardMatrix*)s)->elements[i]; }
Matrix_vtable vt_Std = { 0, Std_size, Std_get };
Matrix as_Matrix(StandardMatrix* p) { return { p, &vt_Std }; }

// HotVector
struct HotVector { int32_t idx; int32_t size; };
int32_t Hot_size(void* h) { return ((HotVector*)h)->size; }
int32_t Hot_get(void* h, int32_t i) { return ((HotVector*)h)->idx == i ? 1 : 0; }
Matrix_vtable vt_Hot = { 1, Hot_size, Hot_get };
Matrix as_Matrix(HotVector* p) { return { p, &vt_Hot }; }

// ─── Multiple dispatch: inner(Matrix, Matrix) ───
using InnerFn = int32_t(*)(void*, void*);
std::unordered_map<uint64_t, InnerFn> _d_inner;

// Register specialization: inner(Matrix, HotVector) → fast path
struct _R {
  _R() { _d_inner[((uint64_t)0 << 16) | 1] = [](void* a, void* b) -> int32_t {
    auto* m = (StandardMatrix*)a; auto* h = (HotVector*)b;
    return m->elements[h->idx] * 2;  // specialized!
  }; }
};
static _R _r;

static struct { uint64_t key = UINT64_MAX; InnerFn fn; } _c_inner;

int32_t inner(Matrix& a, Matrix& b) {
  uint64_t k = ((uint64_t)a._vt->type_id << 16) | b._vt->type_id;
  if (k == _c_inner.key) [[likely]] return _c_inner.fn(a._ptr, b._ptr);
  auto it = _d_inner.find(k);
  if (it != _d_inner.end()) { _c_inner = {k, it->second}; return it->second(a._ptr, b._ptr); }
  return a.size() * b.size();  // default
}

int main() {
  StandardMatrix dense{{1, 2, 3, 4, 5}};
  HotVector hot{2, 10};
  Matrix a = as_Matrix(&dense);
  Matrix b = as_Matrix(&hot);
  std::cout << inner(a, b) << std::endl;  // 6 (specialized)
  std::cout << inner(a, a) << std::endl;  // 25 (default)
  return 0;
}
