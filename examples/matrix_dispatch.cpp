// ─── Generic runtime dispatch — works for any trait function ───
#include <cstdint>
#include <iostream>
#include <unordered_map>

struct Matrix_vtable { uint32_t type_id; int32_t (*size)(void*); };
struct Matrix { void* _ptr; Matrix_vtable* _vt;
  int32_t size() { return _vt->size(_ptr); }
};

struct Dense { int32_t sz; };
int32_t Dense_size(void* s) { return ((Dense*)s)->sz; }
Matrix_vtable vt_Dense = { 0, Dense_size };
Matrix as_Matrix(Dense* p) { return { p, &vt_Dense }; }

struct HotVector { int32_t idx; int32_t sz; };
int32_t Hot_size(void* s) { return ((HotVector*)s)->sz; }
Matrix_vtable vt_Hot = { 1, Hot_size };
Matrix as_Matrix(HotVector* p) { return { p, &vt_Hot }; }

// ─── Dispatch: per-function-name table + per-call-site cache ───
using DispatchKey = uint64_t;
using DispatchFn2 = int(*)(Matrix&, Matrix&);
static std::unordered_map<DispatchKey, DispatchFn2> _table_inner;

struct _Reg {
  _Reg() {
    _table_inner[0ul | 0ul << 16] = [](Matrix& a, Matrix& b) { return a.size() * b.size(); };
    _table_inner[0ul | 1ul << 16] = [](Matrix& a, Matrix& b) { return a.size() + ((HotVector*)b._ptr)->idx; };
    _table_inner[1ul | 0ul << 16] = [](Matrix& a, Matrix& b) { return a.size() * b.size(); };
    _table_inner[1ul | 1ul << 16] = [](Matrix& a, Matrix& b) { return a.size() * b.size(); };
  }
} _reg;

// Per-call-site cache
static struct { DispatchKey key = UINT64_MAX; DispatchFn2 fn; } _cache_inner;

int inner(Matrix a, Matrix b) {
  DispatchKey k = (uint64_t)a._vt->type_id | ((uint64_t)b._vt->type_id << 16);
  if (k == _cache_inner.key) [[likely]] return _cache_inner.fn(a, b);
  auto it = _table_inner.find(k);
  if (it != _table_inner.end()) { _cache_inner = {k, it->second}; return it->second(a, b); }
  return a.size() * b.size();  // fallback default
}

Matrix make_matrix(int kind) {
  if (kind == 0) return as_Matrix(new Dense{5});
  else           return as_Matrix(new HotVector{2, 10});
}

int main() {
  Matrix a = make_matrix(0), b = make_matrix(1);
  std::cout << inner(a, b) << std::endl;  // 7
  std::cout << inner(a, a) << std::endl;  // 25
  return 0;
}
