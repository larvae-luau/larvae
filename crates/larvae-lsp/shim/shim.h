/*
The C surface between Rust and Luau's analysis frontend.

Eight functions, byte offsets in both directions, and two callbacks that
hand require resolution and source loading to Rust. Strings that the shim
returns belong to the session and stay valid until the next call on the
same session; Rust copies what it keeps.
*/

#pragma once
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LarvaeSession LarvaeSession;

/* Rust resolves a require spec from a module; returns a path the loader
   understands, or null when nothing resolves. The buffer belongs to Rust
   and lives until the next resolver call. */
typedef const char* (*larvae_resolve_fn)(void* userdata, const char* from, const char* spec);

/* Rust loads the text of a module path; null when the file is gone. */
typedef const char* (*larvae_load_fn)(void* userdata, const char* path);

LarvaeSession* larvae_session_new(void);
void larvae_session_free(LarvaeSession* s);

void larvae_set_resolver(LarvaeSession* s, void* userdata, larvae_resolve_fn resolve, larvae_load_fn load);

/* Declaration text in .d.luau form, loaded into the global scope. */
int larvae_set_definitions(LarvaeSession* s, const char* name, const char* source);

/* Luau's own feature flags. They are process wide, not per session.

   Apply them in the order luau-lsp applies them: every flag on, then the
   project's overrides, then the values larvae requires. A later step wins,
   which is why the required values go last. */

/* Turn on every non experimental Luau analysis flag. */
void larvae_enable_all_flags(void);

/* Set one flag by name. 0 ok, 1 unknown name, 2 the value does not parse. */
int larvae_set_flag(const char* name, const char* value);

/* Put back the values larvae cannot work without. Call after any override. */
void larvae_apply_required_flags(void);

/* The text of one open module; replaces what the session held. */
void larvae_open(LarvaeSession* s, const char* path, const char* text);

/* Drop the cached state of one module and everything that depends on it. */
void larvae_invalidate(LarvaeSession* s, const char* path);

/* One diagnostic, byte addressed against the module's text. */
typedef struct {
    uint32_t start;
    uint32_t end;
    uint8_t severity; /* 1 error, 2 warning */
    const char* message;
} LarvaeDiag;

/* Type-check one module. Returns how many diagnostics, writes at most cap. */
size_t larvae_check(LarvaeSession* s, const char* path, LarvaeDiag* out, size_t cap);

/* The type at a byte offset, rendered; null when nothing is there.

   `show_table_kinds` keeps the `{| |}` and `{- -}` markers that say whether a
   table is sealed. They are noise to most readers, so the default hides them
   and the setting brings them back. */
const char* larvae_hover(LarvaeSession* s, const char* path, uint32_t byte, int show_table_kinds,
                         int include_string_length);

typedef struct {
    const char* label;
    uint8_t kind; /* CompletionItemKind of the protocol */
} LarvaeCompletion;

/* Completions at a byte offset. Returns how many, writes at most cap. */
size_t larvae_completions(LarvaeSession* s, const char* path, uint32_t byte, LarvaeCompletion* out, size_t cap);

/* Where a name is declared.

   Line and character, and not a byte offset, because the answer often names
   a module the caller has no text for. Those are the units the protocol
   wants anyway, so nothing converts on either side.

   `path` is the module the declaration sits in, and it belongs to the
   session until the next call. */
typedef struct {
    const char* path;
    uint32_t start_line;
    uint32_t start_character;
    uint32_t end_line;
    uint32_t end_character;
} LarvaeLocation;

/* The declaration of whatever sits at a byte offset. 1 on success. */
int larvae_definition(LarvaeSession* s, const char* path, uint32_t byte, LarvaeLocation* out);

/* The declaration of the TYPE of whatever sits at a byte offset. 1 on success. */
int larvae_type_definition(LarvaeSession* s, const char* path, uint32_t byte, LarvaeLocation* out);

/* One parameter of a signature, so the editor can bold the active one. */
typedef struct {
    const char* label;   /* "name: type", or just the type when unnamed */
} LarvaeParameter;

/* The signature of the call that encloses a byte offset.

   `label` is the whole signature as one line. The parameters index into it
   by name, and `active` says which one the caret sits on. Strings belong to
   the session until the next call. */
typedef struct {
    const char* label;
    uint32_t active;
    size_t count;              /* how many parameters exist */
} LarvaeSignature;

/* Fills `sig` and writes at most `cap` parameters. 1 on success. */
int larvae_signature_help(
    LarvaeSession* s, const char* path, uint32_t byte,
    LarvaeSignature* sig, LarvaeParameter* out, size_t cap);

/* One inlay hint: a short label the editor draws inside the line. */
typedef struct {
    uint32_t line;
    uint32_t character;
    const char* label;
    uint8_t kind;    /* 1 type, 2 parameter */
} LarvaeHint;

/* The hints for a whole module. Returns how many, writes at most cap. */
size_t larvae_inlay_hints(LarvaeSession* s, const char* path, LarvaeHint* out, size_t cap);

#ifdef __cplusplus
}
#endif
