// nova_runtime.h — C++ runtime for Nova stdlib functions
// These implement std/io.nv functions at runtime.
// Include this in your C++ project alongside the generated code.

#pragma once

#include <string>
#include <iostream>
#include <fstream>
#include <sstream>

// ─── std/io ──────────────────────────────────────────────────────────────────

inline std::string nova_read_file(const std::string& path) {
    std::ifstream file(path);
    if (!file) return "";
    std::stringstream buf;
    buf << file.rdbuf();
    return buf.str();
}

inline bool nova_file_exists(const std::string& path) {
    std::ifstream f(path);
    return f.good();
}

inline void nova_print(const std::string& msg) {
    std::cout << msg;
}

inline void nova_println(const std::string& msg) {
    std::cout << msg << std::endl;
}

inline void nova_print_int(int n) {
    std::cout << n;
}
