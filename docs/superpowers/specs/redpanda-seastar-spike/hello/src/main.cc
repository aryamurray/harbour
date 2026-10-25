#include <seastar/core/app-template.hh>
#include <seastar/core/coroutine.hh>
#include <seastar/core/sleep.hh>
#include <iostream>

seastar::future<> run() {
    co_await seastar::sleep(std::chrono::milliseconds(10));
    std::cout << "OK seastar reactor ran a coroutine\n";
}

int main(int argc, char **argv) {
    seastar::app_template app;
    return app.run(argc, argv, run);
}
