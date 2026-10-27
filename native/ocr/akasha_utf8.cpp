// Windows UTF-8 traineddata reader for Tesseract's public FileReader API.
// Compiled inside the pinned Tesseract DLL; no C++ object crosses the Rust ABI.
#include <tesseract/baseapi.h>
#include <filesystem>
#include <fstream>
#include <vector>

static bool ReadUtf8(const char* filename, std::vector<char>* bytes) {
  std::ifstream file(std::filesystem::u8path(filename), std::ios::binary | std::ios::ate);
  if (!file) return false;
  const auto size = file.tellg();
  if (size <= 0 || size > 32 * 1024 * 1024) return false;
  bytes->resize(static_cast<size_t>(size));
  file.seekg(0);
  return static_cast<bool>(file.read(bytes->data(), size));
}

extern "C" __declspec(dllexport) int AkashaTessInitUtf8(
    tesseract::TessBaseAPI* api, const char* directory, const char* languages) {
  try {
    return api->Init(directory, 0, languages, tesseract::OEM_DEFAULT,
                     nullptr, 0, nullptr, nullptr, false, ReadUtf8);
  } catch (...) {
    return -1;
  }
}
