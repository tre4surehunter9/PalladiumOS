// Copyright (c) 2026 tre4surehunter9
// test_cxx: C++ on PalladiumOS (palladium-c++, libstdc++): containers, smart
// pointers, exceptions across functions, RTTI, std::thread and std::mutex,
// <regex>, <chrono>, std::format, static constructors. Prints "c++ ok".
#include <iostream>
#include <string>
#include <vector>
#include <map>
#include <memory>
#include <stdexcept>
#include <thread>
#include <mutex>
#include <regex>
#include <chrono>
#include <format>
#include <sstream>
#include <algorithm>
struct Shape { virtual ~Shape() = default; virtual double area() const = 0; };
struct Square : Shape { double s; Square(double s) : s(s) {} double area() const override { return s * s; } };
static std::string made = [] { return std::string("static init ok"); }();
int thrower(int n) { if (n > 2) throw std::runtime_error("too big: " + std::to_string(n)); return n; }
int main() {
    std::vector<std::unique_ptr<Shape>> v; v.push_back(std::make_unique<Square>(3));
    std::map<std::string, int> m{{"b", 2}, {"a", 1}};
    int caught = 0;
    try { thrower(5); } catch (const std::exception &e) { caught = std::string(e.what()) == "too big: 5"; }
    Shape *s = v[0].get();
    bool rtti = dynamic_cast<Square *>(s) != nullptr && typeid(*s) == typeid(Square);
    std::mutex mu; int total = 0; std::vector<std::thread> ts;
    for (int i = 1; i <= 4; i++) ts.emplace_back([&, i] { std::lock_guard<std::mutex> g(mu); total += i; });
    for (auto &t : ts) t.join();
    std::smatch sm; std::string text = "PalladiumOS 0.6.8";
    bool re = std::regex_search(text, sm, std::regex(R"((\d+)\.(\d+)\.(\d+))")) && sm[3] == "8";
    auto t0 = std::chrono::steady_clock::now(); std::this_thread::sleep_for(std::chrono::milliseconds(20));
    bool clock = std::chrono::steady_clock::now() - t0 >= std::chrono::milliseconds(15);
    std::ostringstream os; os << std::format("{} {:.1f} {}", m.begin()->first, s->area(), total);
    std::vector<int> nums{5, 3, 9, 1}; std::sort(nums.begin(), nums.end());
    bool ok = caught && rtti && total == 10 && re && clock && os.str() == "a 9.0 10" && nums[0] == 1 && made == "static init ok";
    std::cout << (ok ? "c++ ok" : "c++ FAILED") << " (" << os.str() << ")" << std::endl;
    return ok ? 0 : 1;
}
