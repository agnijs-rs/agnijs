# Builds the C test program and the C and C++ examples against the AgniJS C
# API, linked both statically and dynamically. Build the Rust library first:
#
#   cargo build -p agnijs-capi --release
#   cmake -S crates/agnijs-capi/c -B build/capi -DAGNIJS_LIB_DIR=<target>/release
#   cmake --build build/capi --config Release
#   ctest --test-dir build/capi -C Release --output-on-failure
cmake_minimum_required(VERSION 3.16)
project(agnijs_capi_c C CXX)

set(CMAKE_C_STANDARD 99)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(AGNIJS_LIB_DIR "" CACHE PATH "Cargo output directory holding the agnijs_capi libraries")
if(NOT AGNIJS_LIB_DIR)
  message(FATAL_ERROR "Set AGNIJS_LIB_DIR to the cargo target directory (for example target/release)")
endif()
set(AGNIJS_INCLUDE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../include")

if(MSVC)
  add_compile_options(/W4 /WX)
else()
  add_compile_options(-Wall -Wextra -Werror)
endif()

# The static library and the system libraries the Rust standard library
# needs (`cargo rustc -- --print native-static-libs`).
if(WIN32)
  set(AGNIJS_STATIC "${AGNIJS_LIB_DIR}/agnijs_capi.lib")
  set(AGNIJS_SHARED_IMPLIB "${AGNIJS_LIB_DIR}/agnijs_capi.dll.lib")
  set(AGNIJS_SHARED "${AGNIJS_LIB_DIR}/agnijs_capi.dll")
  set(AGNIJS_SYSTEM_LIBS kernel32 ntdll userenv ws2_32 dbghelp advapi32 bcrypt)
elseif(APPLE)
  set(AGNIJS_STATIC "${AGNIJS_LIB_DIR}/libagnijs_capi.a")
  set(AGNIJS_SHARED "${AGNIJS_LIB_DIR}/libagnijs_capi.dylib")
  set(AGNIJS_SYSTEM_LIBS "-framework CoreFoundation" "-framework Security" iconv m)
else()
  set(AGNIJS_STATIC "${AGNIJS_LIB_DIR}/libagnijs_capi.a")
  set(AGNIJS_SHARED "${AGNIJS_LIB_DIR}/libagnijs_capi.so")
  find_package(Threads REQUIRED)
  set(AGNIJS_SYSTEM_LIBS Threads::Threads ${CMAKE_DL_LIBS} m)
endif()

add_library(agnijs_static STATIC IMPORTED)
set_target_properties(agnijs_static PROPERTIES
  IMPORTED_LOCATION "${AGNIJS_STATIC}"
  INTERFACE_INCLUDE_DIRECTORIES "${AGNIJS_INCLUDE_DIR}"
  INTERFACE_LINK_LIBRARIES "${AGNIJS_SYSTEM_LIBS}")

add_library(agnijs_shared SHARED IMPORTED)
set_target_properties(agnijs_shared PROPERTIES
  IMPORTED_LOCATION "${AGNIJS_SHARED}"
  INTERFACE_INCLUDE_DIRECTORIES "${AGNIJS_INCLUDE_DIR}")
if(WIN32)
  set_target_properties(agnijs_shared PROPERTIES IMPORTED_IMPLIB "${AGNIJS_SHARED_IMPLIB}")
endif()

enable_testing()

function(agnijs_program name source)
  foreach(kind static shared)
    add_executable(${name}_${kind} ${source})
    target_link_libraries(${name}_${kind} PRIVATE agnijs_${kind})
    if(kind STREQUAL "shared")
      if(WIN32)
        # Put the DLL next to the program so it is found at run time.
        add_custom_command(TARGET ${name}_${kind} POST_BUILD
          COMMAND ${CMAKE_COMMAND} -E copy_if_different "${AGNIJS_SHARED}" $<TARGET_FILE_DIR:${name}_${kind}>)
      else()
        set_target_properties(${name}_${kind} PROPERTIES BUILD_RPATH "${AGNIJS_LIB_DIR}")
      endif()
    endif()
    add_test(NAME ${name}_${kind} COMMAND ${name}_${kind})
  endforeach()
endfunction()

agnijs_program(capi_test tests/capi_test.c)
agnijs_program(example_c examples/timers.c)
agnijs_program(example_cpp examples/classes.cpp)
