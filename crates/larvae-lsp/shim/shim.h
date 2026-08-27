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

/* Put every boolean flag back to the value it had at startup. The flags are
   global to the process, so a caller that changed them owes this to whoever
   builds the next session. */
void larvae_reset_flags(void);

/* The text of one open module; replaces what the session held. */
void larvae_open(LarvaeSession* s, const char* path, const char* text);

/* Drop the cached state of one module and everything that depends on it. */
void larvae_invalidate(LarvaeSession* s, const char* path);

/* The type that `script` takes inside one module.

   `script` names a different instance in every file, so a global declaration
   cannot say what it is. The sourcemap can, and these two carry the answer:
   clear drops the whole map, set names the declared type of one file. Both
   mark the modules they touch dirty, so the next check reads the new type. */
void larvae_clear_script_types(LarvaeSession* s);
void larvae_set_script_type(LarvaeSession* s, const char* path, const char* type_name);

/* One diagnostic, byte addressed against the module's text.

   `code` is Luau's own error number, which starts at 1000 and names the
   kind of the error. It is 0 for a finding that carries no number, ex: a
   syntax error. An editor shows it beside the message and links it. */
typedef struct {
    uint32_t start;
    uint32_t end;
    int32_t code;
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

/* The documentation symbol at a byte offset, or null.

   It names an entry of the Roblox documentation database, which Rust holds
   and looks up. The two are split because the database is 19000 entries of
   JSON, and a JSON parser does not belong in this shim. */
const char* larvae_documentation_symbol(LarvaeSession* s, const char* path, uint32_t byte);

typedef struct {
    const char* label;
    /* The type of the entry, rendered. Null for a keyword, which has none.
       An editor shows it beside the label, which is how a reader tells a
       function from a field without accepting either. */
    const char* detail;
    /* The parameter names of a function, ex: `(self, className)`. An editor
       draws it against the label itself, before the detail. Null for
       anything that is not a function. */
    const char* label_detail;
    /* What the editor writes when the entry is accepted, when that differs
       from the label: a function takes its parentheses. */
    const char* insert_text;
    /* The comment block above the declaration, as markdown. Null when the
       entry has no declaration this session can read. */
    const char* documentation;
    /* The documentation symbol of the entry, ex: `@roblox/globaltype/Player`.
       Rust holds the documentation database and looks this up. Null when the
       entry names nothing the database can answer. */
    const char* documentation_symbol;
    uint8_t kind; /* CompletionItemKind of the protocol */
    uint8_t deprecated; /* 1 when the declaration carries @deprecated */
    /* Whether the entry fits the type the position expects: 0 no, 1 yes,
       2 a function whose result fits. It is what ranks a table key above
       every global in scope, which is the difference between a useful list
       and an alphabet. */
    uint8_t type_correct;
    /* 1 when the entry is reached through an index the type does not take,
       ex: a property read off a metatable index. It ranks last. */
    uint8_t wrong_index_type;
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
