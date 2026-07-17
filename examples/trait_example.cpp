// ─── Multiple dispatch trait system — working example ───
#include <cstdint>
#include <iostream>
#include <vector>
#include <unordered_map>

static constexpr uint32_t StandardMatrix_type_id = 0;
static constexpr uint32_t HotVector_type_id = 1;

// ─── trait Matrix ───
struct Matrix_vtable {
  uint32_t type_id;
  int32_t (*size)(void*);
  int32_t (*get)(void*, int32_t);
};

struct Matrix {
  void* _ptr;
  Matrix_vtable* _vt;
  int32_t size() { return _vt->size(_ptr); }
  int32_t get(int32_t idx) { return _vt->get(_ptr, idx); }
};

// ─── StandardMatrix ───
struct StandardMatrix {
  std::vector<int32_t> elements;
  StandardMatrix(std::initializer_list<int32_t> el) : elements(el) {}
};
int32_t Std_size(void* self) { return ((StandardMatrix*)self)->elements.size(); }
int32_t Std_get(void* self, int32_t i) { return ((StandardMatrix*)self)->elements[i]; }

Matrix_vtable __Matrix_vt_Std = { StandardMatrix_type_id, Std_size, Std_get };
Matrix as_Matrix(StandardMatrix* p) { return { p, &__Matrix_vt_Std }; }

// ─── HotVector ───
struct HotVector {
  int32_t idx;
  int32_t size;
};
int32_t Hot_size(void* self) { return ((HotVector*)self)->size; }
int32_t Hot_get(void* self, int32_t i) { return ((HotVector*)self)->idx == i ? 1 : 0; }

Matrix_vtable __Matrix_vt_Hot = { HotVector_type_id, Hot_size, Hot_get };
Matrix as_Matrix(HotVector* p) { return { p, &__Matrix_vt_Hot }; }

// ─── Multiple dispatch for inner(a: Matrix, b: Matrix) ───
using InnerFn = int32_t(*)(void*, void*);
static std::unordered_map<uint64_t, InnerFn> _dispatch_inner;

// Register specialization: inner(StandardMatrix, HotVector) → fast path
struct _Reg {
  _Reg() {
    _dispatch_inner[((uint64_t)StandardMatrix_type_id << 16) | HotVector_type_id] =
      [](void* a, void* b) -> int32_t {
        auto* dense = (StandardMatrix*)a;
        auto* hot = (HotVector*)b;
        return dense->elements[hot->idx] * 2;  // fast path
      };
  }
};
static _Reg _reg;

// Per-call-site cache
static struct { uint64_t key = UINT64_MAX; InnerFn fn; } _cache;

int32_t inner(Matrix& a, Matrix& b) {
  uint64_t k = ((uint64_t)a._vt->type_id << 16) | b._vt->type_id;
  if (k == _cache.key) [[likely]] return _cache.fn(a._ptr, b._ptr);
  auto it = _dispatch_inner.find(k);
  if (it != _dispatch_inner.end()) {
    _cache.key = k; _cache.fn = it->second;
    return it->second(a._ptr, b._ptr);
  }
  return a.size() * b.size();  // default impl
}

int main() {
  StandardMatrix dense{1, 2, 3, 4, 5};
  HotVector hot{2, 10};

  Matrix a = as_Matrix(&dense);
  Matrix b = as_Matrix(&hot);

  std::cout << "a.size() = " << a.size() << std::endl;        // 5
  std::cout << "a.get(2) = " << a.get(2) << std::endl;        // 3
  std::cout << "b.get(2) = " << b.get(2) << std::endl;        // 1
  std::cout << "inner(a, a) = " << inner(a, a) << std::endl;  // 25 (generic)
  std::cout << "inner(a, b) = " << inner(a, b) << std::endl;  // 6  (specialized!)
  return 0;
}
