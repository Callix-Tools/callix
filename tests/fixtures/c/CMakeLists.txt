cmake_minimum_required(VERSION 3.20)
project(fixture C)

set(CMAKE_C_STANDARD 11)

add_library(engine src/engine.c)
target_include_directories(engine PUBLIC include)

add_executable(fixture src/main.c)
target_link_libraries(fixture PRIVATE engine curl)
