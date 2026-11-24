#include "tree_sitter/parser.h"
#include <string.h>

enum Token {
  LINE_COMMENT, RAW_STRING, RAW_BYTE_STRING,
  EXPRESSION_NEWLINE, ENTRY_NEWLINE, EXIT_NEWLINE, TRIVIA_NEWLINE,
  ITEM_NEWLINE, STEPS_MODE, PROJECTION_DOT, ERROR_RECOVERY, BYTES_MODE,
};

static void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

static bool forbidden(int32_t c) {
  return c == 0x061c || c == 0x200e || c == 0x200f ||
         (c >= 0x202a && c <= 0x202e) || (c >= 0x2066 && c <= 0x2069);
}

static bool newline(TSLexer *lexer) {
  if (lexer->lookahead == '\r') advance(lexer);
  if (lexer->lookahead != '\n') return false;
  advance(lexer);
  return true;
}

// Every line tokenizes on its own (LANG-1): `//` is the only comment, and it ends at the line end.
static bool comment(TSLexer *lexer) {
  advance(lexer);  // The caller consumed the opening slash.
  while (!lexer->eof(lexer) && lexer->lookahead != '\n' && lexer->lookahead != '\r') {
    if (forbidden(lexer->lookahead)) return false;
    advance(lexer);
  }
  return true;
}

static bool raw_string(TSLexer *lexer, bool bytes) {
  advance(lexer);
  unsigned hashes = 0;
  while (lexer->lookahead == '#') {
    advance(lexer);
    hashes++;
  }
  if (lexer->lookahead != '"') return false;
  advance(lexer);

  while (!lexer->eof(lexer)) {
    const int32_t c = lexer->lookahead;
    if (c == '\r' || c == '\n' || forbidden(c) || (bytes && c > 127)) return false;
    advance(lexer);
    if (c != '"') continue;
    unsigned closing = 0;
    while (closing < hashes && lexer->lookahead == '#') {
      advance(lexer);
      closing++;
    }
    if (closing == hashes) {
      lexer->result_symbol = bytes ? RAW_BYTE_STRING : RAW_STRING;
      lexer->mark_end(lexer);
      return true;
    }
  }
  return false;
}

static bool name_character(int32_t c) {
  return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
         (c >= '0' && c <= '9') || c == '_';
}

static bool keyword(TSLexer *lexer, const char *word) {
  for (const char *p = word; *p; p++) {
    if (lexer->lookahead != *p) return false;
    advance(lexer);
  }
  return !name_character(lexer->lookahead);
}

static bool continues_expression(TSLexer *lexer, int32_t next) {
  switch (next) {
    case '&': case '|': case '=': case '<': case '>':
    case '+': case '*': case '/': case '%': case '^': case '.': case '?':
    case ',': case '}':  // The parser accepts the delimiter without an END.
      return true;
    case '-': case '!':
      advance(lexer);
      return lexer->lookahead == '=';
    case 'e':
      return keyword(lexer, "else");
    default:
      return false;
  }
}

void *tree_sitter_hirsel_external_scanner_create(void) { return NULL; }
void tree_sitter_hirsel_external_scanner_destroy(void *payload) { (void)payload; }
unsigned tree_sitter_hirsel_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload; (void)buffer;
  return 0;
}
void tree_sitter_hirsel_external_scanner_deserialize(void *payload, const char *buffer,
                                                     unsigned length) {
  (void)payload; (void)buffer; (void)length;
}

static int32_t next_token(TSLexer *lexer) {
  for (;;) {
    int32_t c = lexer->lookahead;
    if (c == ' ' || c == '\t') {
      advance(lexer);
    } else if (c == '\r' || c == '\n') {
      if (!newline(lexer)) return c;
    } else if (c == '/') {
      advance(lexer);
      if (lexer->lookahead != '/' || !comment(lexer)) return c;
    } else {
      return c;
    }
  }
}

bool tree_sitter_hirsel_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid) {
  (void)payload;
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t') lexer->advance(lexer, true);
  int32_t c = lexer->lookahead;
  if (c == 'r' && valid[RAW_STRING]) return raw_string(lexer, false);
  if (c == 'b' && (valid[RAW_BYTE_STRING] || valid[BYTES_MODE])) {
    advance(lexer);
    if (lexer->lookahead == 'r' && valid[RAW_BYTE_STRING]) return raw_string(lexer, true);
    if (!valid[BYTES_MODE] || valid[ERROR_RECOVERY] || !keyword(lexer, "ytes")) return false;
    lexer->mark_end(lexer);
    if (next_token(lexer) == '[') return false;
    lexer->result_symbol = BYTES_MODE;
    return true;
  }
  if (c == 's' && valid[STEPS_MODE] && !valid[ERROR_RECOVERY]) {
    if (!keyword(lexer, "steps")) return false;
    lexer->mark_end(lexer);
    if (!name_character(next_token(lexer))) return false;
    lexer->result_symbol = STEPS_MODE;
    return true;
  }

  if (c == '.' && valid[PROJECTION_DOT] && !valid[ERROR_RECOVERY]) {
    advance(lexer);
    lexer->mark_end(lexer);
    if (!name_character(next_token(lexer))) return false;
    lexer->result_symbol = PROJECTION_DOT;
    return true;
  }

  lexer->mark_end(lexer);
  if (c == '/') {
    advance(lexer);
    if (lexer->lookahead != '/' || !valid[LINE_COMMENT] || !comment(lexer)) return false;
    lexer->mark_end(lexer);
    lexer->result_symbol = LINE_COMMENT;
    return true;
  }
  if ((c != '\r' && c != '\n') || !newline(lexer)) return false;

  if (valid[EXIT_NEWLINE] && !valid[ERROR_RECOVERY]) {
    lexer->result_symbol = EXIT_NEWLINE;
    return true;  // Leave the newline for the enclosing statement.
  }
  lexer->mark_end(lexer);
  if (!valid[ERROR_RECOVERY] &&
      (valid[EXPRESSION_NEWLINE] || valid[ENTRY_NEWLINE] || valid[ITEM_NEWLINE])) {
    int32_t next = next_token(lexer);
    if (valid[EXPRESSION_NEWLINE] && !continues_expression(lexer, next)) {
      lexer->result_symbol = EXPRESSION_NEWLINE;
      return true;
    }
    // Only an item starter needs an item separator. In particular, a header
    // can keep going at a body, where clause, or other header punctuation.
    if (valid[ITEM_NEWLINE] && next != 0 && next < 128 && strchr("@pcftrs", next)) {
      lexer->result_symbol = ITEM_NEWLINE;
      return true;
    }
    if (valid[ENTRY_NEWLINE] && next != '=' && next != ',' && next != '}') {
      lexer->result_symbol = ENTRY_NEWLINE;
      return true;
    }
  }

  lexer->result_symbol = TRIVIA_NEWLINE;
  return valid[TRIVIA_NEWLINE];
}
