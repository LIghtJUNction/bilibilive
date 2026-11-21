cmake_minimum_required(VERSION 3.28)
project(obs-bilibilive-pk VERSION 0.3.0 LANGUAGES CXX)

find_package(Threads REQUIRED)
# Older Linux libc versions provide shm_open in librt; macOS has it in libc.
if(UNIX AND NOT APPLE)
  link_libraries(rt)
endif()
option(STARTLIVE_PK_BUILD_PLUGIN "Build the OBS plugin (requires the OBS SDK)" ON)
if(STARTLIVE_PK_BUILD_PLUGIN)
  set(pk_max_fps 60 CACHE STRING "Maximum PK capture frame rate")
  # Release binaries use one fixed SDK baseline; Linux uses its native SDK.
  if(WIN32 OR APPLE)
    find_package(libobs CONFIG REQUIRED)
    find_package(obs-frontend-api CONFIG REQUIRED)
    find_package(Qt6 REQUIRED COMPONENTS Widgets)
  else()
    find_package(libobs CONFIG REQUIRED)
    find_package(obs-frontend-api CONFIG REQUIRED)
    find_package(Qt6 REQUIRED COMPONENTS Widgets)
  endif()
  # Qt 6.3.1 assumes OpenGL's IMPORTED_LOCATION names a binary; CMake 4.2
  # names the framework directory, so Qt mistakenly links its parent folder.
  # Use CMake's target directly. This also omits AGL, a no-op on 64-bit macOS
  # that was removed from the macOS 26 SDK, while retaining OpenGL itself.
  if(APPLE)
    set_property(TARGET WrapOpenGL::WrapOpenGL PROPERTY INTERFACE_LINK_LIBRARIES OpenGL::GL)
  endif()
  add_library(obs-bilibilive-pk MODULE src/plugin.cpp src/media_transport.cpp src/shared_video.cpp)
  target_compile_features(obs-bilibilive-pk PRIVATE cxx_std_17)
  target_compile_definitions(obs-bilibilive-pk PRIVATE STARTLIVE_PK_MAX_FPS=${pk_max_fps})
  target_link_libraries(obs-bilibilive-pk PRIVATE OBS::libobs OBS::obs-frontend-api Threads::Threads Qt6::Widgets)
  if(MSVC)
    target_compile_options(obs-bilibilive-pk PRIVATE /utf-8)
  endif()
  if(WIN32)
    # OBS 28 requires Windows 10. A newer build SDK does not change the
    # Windows API level requested by this plugin.
    target_compile_definitions(obs-bilibilive-pk PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX
      WINVER=0x0A00 _WIN32_WINNT=0x0A00 NTDDI_VERSION=0x0A000000)
    target_link_libraries(obs-bilibilive-pk PRIVATE advapi32)
  endif()
  set_target_properties(obs-bilibilive-pk PROPERTIES PREFIX "")
  if(APPLE)
    set_target_properties(obs-bilibilive-pk PROPERTIES BUNDLE TRUE BUNDLE_EXTENSION plugin SUFFIX ""
      XCODE_ATTRIBUTE_PRODUCT_BUNDLE_IDENTIFIER "io.github.lightjunction.obs-bilibilive-pk"
      # Resolve OBS/Qt frameworks inside the host OBS.app after installation.
      INSTALL_RPATH "@executable_path/../Frameworks"
      MACOSX_BUNDLE_INFO_PLIST "${CMAKE_CURRENT_SOURCE_DIR}/src/Info.plist.in")
    # MODULE bundles use the LIBRARY artifact kind; BUNDLE is for .app executables.
    install(TARGETS obs-bilibilive-pk LIBRARY DESTINATION .)
  elseif(WIN32)
    install(TARGETS obs-bilibilive-pk RUNTIME DESTINATION obs-plugins/64bit
            LIBRARY DESTINATION obs-plugins/64bit)
  else()
    include(GNUInstallDirs)
    install(TARGETS obs-bilibilive-pk LIBRARY DESTINATION ${CMAKE_INSTALL_LIBDIR}/obs-plugins)
  endif()
endif()
