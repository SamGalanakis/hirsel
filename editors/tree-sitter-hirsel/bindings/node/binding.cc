#include <napi.h>

struct TSLanguage;
extern "C" const TSLanguage *tree_sitter_hirsel();
extern "C" const TSLanguage *tree_sitter_hirsel_data();

// The tree-sitter Node binding requires this type tag for its language object.
const napi_type_tag LANGUAGE_TYPE_TAG = {0x8AF2E5212AD58ABF, 0xD5006CAD83ABBA16};

Napi::Object Init(Napi::Env env, Napi::Object exports) {
  auto language = Napi::External<TSLanguage>::New(env, const_cast<TSLanguage *>(tree_sitter_hirsel()));
  language.TypeTag(&LANGUAGE_TYPE_TAG);
  exports["language"] = language;
  auto data = Napi::External<TSLanguage>::New(env, const_cast<TSLanguage *>(tree_sitter_hirsel_data()));
  data.TypeTag(&LANGUAGE_TYPE_TAG);
  auto config = Napi::Object::New(env);
  config["language"] = data;
  exports["data"] = config;
  return exports;
}

NODE_API_MODULE(tree_sitter_hirsel_binding, Init)
