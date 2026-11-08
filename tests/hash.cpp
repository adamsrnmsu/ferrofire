// Oracle for the RADIO/DISH group hash: prints libstdc++'s std::hash<std::string> for the spec vectors.
// docker run --rm -v $PWD/tests:/w:ro <image with g++> bash -c 'g++ -std=c++17 -o /tmp/h /w/hash.cpp && /tmp/h'
#include <functional>
#include <iostream>
#include <string>
int main() {
    for (std::string s : {"udp://239.0.0.1:40000", "udp://127.0.0.1:5556"})
        std::cout << s << " " << std::hex << std::hash<std::string>{}(s) << "\n";
}
