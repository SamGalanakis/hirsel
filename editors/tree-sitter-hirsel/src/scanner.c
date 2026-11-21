#include "tree_sitter/parser.h"
#include <string.h>

enum Token {
  LINE_COMMENT, BLOCK_COMMENT, RAW_STRING, RAW_BYTE_STRING,
  EXPRESSION_NEWLINE, ENTRY_NEWLINE, EXIT_NEWLINE, TRIVIA_NEWLINE,
  EXPRESSION_COMMENT, ENTRY_COMMENT, ITEM_NEWLINE, ITEM_COMMENT, STEPS_MODE, PROJECTION_DOT, ERROR_RECOVERY,
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

static bool comment(TSLexer *lexer, enum Token kind, bool *line) {
  advance(lexer);  // The caller consumed the opening slash.
  if (kind == LINE_COMMENT) {
    while (!lexer->eof(lexer) && lexer->lookahead != '\n' && lexer->lookahead != '\r') {
      if (forbidden(lexer->lookahead)) return false;
      advance(lexer);
    }
    return true;
  }

  unsigned depth = 1;
  while (!lexer->eof(lexer)) {
    const int32_t c = lexer->lookahead;
    if (forbidden(c)) return false;
    if (c == '\r' || c == '\n') {
      if (!newline(lexer)) return false;
      *line = true;
      continue;
    }
    advance(lexer);
    if (c == '/' && lexer->lookahead == '*') {
      advance(lexer);
      depth++;
    } else if (c == '*' && lexer->lookahead == '/') {
      advance(lexer);
      if (--depth == 0) return true;
    }
  }
  return false;
}

static bool raw_string(TSLexer *lexer, bool bytes) {
  advance(lexer);
  if (bytes) {
    if (lexer->lookahead != 'r') return false;
    advance(lexer);
  }
  unsigned hashes = 0;
  while (lexer->lookahead == '#') {
    advance(lexer);
    hashes++;
  }
  if (lexer->lookahead != '"') return false;
  advance(lexer);

  while (!lexer->eof(lexer)) {
    const int32_t c = lexer->lookahead;
    if (c == '\r' || forbidden(c) || (bytes && c > 127)) return false;
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
      int32_t delimiter = lexer->lookahead;
      if (delimiter != '/' && delimiter != '*') return c;
      bool line = false;
      if (!comment(lexer, delimiter == '/' ? LINE_COMMENT : BLOCK_COMMENT, &line)) return c;
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
  if (c == 'b' && valid[RAW_BYTE_STRING]) return raw_string(lexer, true);
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

  // A multiline block comment can separate entries without losing its comment node.
  bool line = false;
  enum Token trivia = TRIVIA_NEWLINE;
  lexer->mark_end(lexer);
  if (c == '/') {
    advance(lexer);
    if (lexer->lookahead == '*') trivia = BLOCK_COMMENT;
    else if (lexer->lookahead == '/') trivia = LINE_COMMENT;
    else return false;
    if (!valid[trivia] || !comment(lexer, trivia, &line)) return false;
  } else {
    if ((c != '\r' && c != '\n') || !newline(lexer)) return false;
    line = true;
  }

  if (line && valid[EXIT_NEWLINE] && !valid[ERROR_RECOVERY]) {
    lexer->result_symbol = EXIT_NEWLINE;
    return true;  // Leave the newline for the enclosing statement.
  }
  lexer->mark_end(lexer);
  enum Token expression_end = trivia == BLOCK_COMMENT ? EXPRESSION_COMMENT : EXPRESSION_NEWLINE;
  enum Token entry_end = trivia == BLOCK_COMMENT ? ENTRY_COMMENT : ENTRY_NEWLINE;
  enum Token item_end = trivia == BLOCK_COMMENT ? ITEM_COMMENT : ITEM_NEWLINE;
  if (line && !valid[ERROR_RECOVERY] &&
      (valid[expression_end] || valid[entry_end] || valid[item_end])) {
    int32_t next = next_token(lexer);
    if (valid[expression_end] && !continues_expression(lexer, next)) {
      lexer->result_symbol = expression_end;
      return true;
    }
    // Only an item starter needs an item separator. In particular, a header
    // can keep going at a body, where clause, or other header punctuation.
    if (valid[item_end] && next != 0 && next < 128 && strchr("@pcftrs", next)) {
      lexer->result_symbol = item_end;
      return true;
    }
    if (valid[entry_end] && next != '=' && next != ',' && next != '}') {
      lexer->result_symbol = entry_end;
      return true;
    }
  }

  lexer->result_symbol = trivia;
  return valid[trivia];
}
