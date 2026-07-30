cmake_minimum_required(VERSION 3.20)
project(fixture CXX)

set(CMAKE_CXX_STANDARD 20)

add_library(engine src/engine.cpp)
target_include_directories(engine PUBLIC include)

add_executable(fixture src/main.cpp)
target_link_libraries(fixture PRIVATE engine curl)
