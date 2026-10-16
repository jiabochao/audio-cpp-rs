# Injected into audio.cpp's top-level project with CMAKE_PROJECT_AudioCpp_INCLUDE.
#
# ggml's Vulkan shader generator is an ExternalProject whose default build
# path repeats the project name several levels deep. On Windows that exceeds
# MAX_PATH for MSBuild's file tracker (error FTK1011). Keep every external
# project directly under the build root instead.
set_property(DIRECTORY PROPERTY EP_BASE "${CMAKE_BINARY_DIR}/ep")
