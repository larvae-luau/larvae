/*
The shim over Luau's analysis frontend.

One session owns one Frontend, a file resolver that calls back into Rust,
and the text of every open module. Positions convert here and nowhere
else: Luau speaks lines and columns, the boundary speaks bytes, and the
session holds the line index of each module it opened.

The strings the shim returns live in per-session buffers that the next
call on the same session overwrites. Rust copies what it keeps, and the
session is used from one thread, which the Rust side guarantees.
*/

#include "shim.h"

#include "Luau/AstQuery.h"
#include "Luau/Scope.h"
#include "Luau/Autocomplete.h"
#include "Luau/ConfigResolver.h"
#include "Luau/BuiltinDefinitions.h"
#include "Luau/Frontend.h"
#include "Luau/ToString.h"
#include "Luau/TypeAttach.h"

#include "Luau/Common.h"
#include "Luau/ExperimentalFlags.h"

#include <cstdlib>
#include <cstring>
#include <map>
#include <utility>
#include <optional>
#include <string>
#include <vector>

namespace
{

struct RustFileResolver : Luau::FileResolver
{
    void* userdata = nullptr;
    larvae_resolve_fn resolve = nullptr;
    larvae_load_fn load = nullptr;
    std::map<std::string, std::string>* open;

    std::optional<Luau::SourceCode> readSource(const Luau::ModuleName& name) override
    {
        auto it = open->find(name);
        if (it != open->end())
            return Luau::SourceCode{it->second, Luau::SourceCode::Module};

        if (load)
        {
            const char* text = load(userdata, name.c_str());
            if (text)
                return Luau::SourceCode{std::string(text), Luau::SourceCode::Module};
        }

        return std::nullopt;
    }

    std::optional<Luau::ModuleInfo> resolveModule(
        const Luau::ModuleInfo* context, Luau::AstExpr* node, const Luau::TypeCheckLimits&) override
    {
        if (!resolve || !context)
            return std::nullopt;

        auto* expr = node->as<Luau::AstExprConstantString>();
        if (!expr)
            return std::nullopt;

        std::string spec(expr->value.data, expr->value.size);
        const char* target = resolve(userdata, context->name.c_str(), spec.c_str());
        if (!target)
            return std::nullopt;

        return Luau::ModuleInfo{target};
    }
};

/* Byte offset <-> Luau Position, against one module's text. */
struct LineIndex
{
    std::vector<uint32_t> starts;

    explicit LineIndex(const std::string& text)
    {
        starts.push_back(0);
        for (uint32_t i = 0; i < text.size(); ++i)
            if (text[i] == '\n')
                starts.push_back(i + 1);
    }

    uint32_t byteOf(const Luau::Position& p, const std::string& text) const
    {
        uint32_t line = p.line < starts.size() ? starts[p.line] : (uint32_t)text.size();
        uint32_t byte = line + p.column;
        return byte > text.size() ? (uint32_t)text.size() : byte;
    }

    Luau::Position positionOf(uint32_t byte) const
    {
        uint32_t line = 0;
        for (size_t i = 0; i < starts.size(); ++i)
            if (starts[i] <= byte)
                line = (uint32_t)i;
            else
                break;
        return Luau::Position{line, byte - starts[line]};
    }
};

} // namespace

/*
The frontend keeps the type graphs it builds, or a hover has nothing to
find: the default discards a module's types the moment check returns.
*/
static Luau::FrontendOptions options()
{
    Luau::FrontendOptions o;
    o.retainFullTypeGraphs = true;
    return o;
}

/*
Set one Luau integer flag by name, because the linker cannot give the symbol.

The flag lives in the vendored archive and the shim is a separate shared
object, so naming the variable fails at the link. Luau registers every flag
in a list it walks itself, and the list is reachable from here.
*/
static void setLuauInt(const char* flag, int value)
{
    for (Luau::FValue<int>* it = Luau::FValue<int>::list; it; it = it->next)
    {
        if (strcmp(it->name, flag) == 0)
        {
            it->value = value;

            return;
        }
    }
}

/*
The two flag values a Luau language server needs, which luau-lsp sets too.

`LuauTarjanChildLimit` guards the type graph walk at 10000 children. The
Roblox type file crosses it, and Luau then refuses the whole file with one
error, `Code is too complex to typecheck`, reported at line 1 of 19728. So
`game` had no type, every Roblox completion came from nothing, and the load
failed quietly. 15000 is the number luau-lsp settles on for the same file.

`LuauTableTypeMaximumStringifierLength` cuts a rendered table type at 40
characters. That is Studio's number, chosen for a panel that cannot scroll.
An editor hover can, so zero lifts the cut and a table type reads in full.
*/
static void applyRequiredFlags()
{
    setLuauInt("LuauTarjanChildLimit", 15000);
    setLuauInt("LuauTableTypeMaximumStringifierLength", 0);
}

/*
Every Luau analysis flag on, minus the ones Luau marks experimental.

This is what luau-lsp does when its `fflags.enableByDefault` is on, and it
is on by default there. Luau ships a change behind a flag long before the
flag flips, so a language server that reads only the defaults reads an
older Luau than the one it links.

An experimental flag stays off. Luau names them for exactly this reason:
they are not ready to be read as behaviour.
*/
static void enableAllFlags()
{
    for (Luau::FValue<bool>* it = Luau::FValue<bool>::list; it; it = it->next)
    {
        if (strncmp(it->name, "Luau", 4) == 0 && !Luau::isAnalysisFlagExperimental(it->name))
            it->value = true;
    }
}

struct LarvaeSession
{
    RustFileResolver files;
    Luau::NullConfigResolver configs;
    Luau::Frontend frontend;

    std::map<std::string, std::string> open;
    std::vector<std::string> diagStorage;
    std::vector<std::string> completionStorage;
    std::string hoverStorage;
    std::string locationStorage;
    std::string signatureStorage;
    std::vector<std::string> parameterStorage;
    std::vector<std::string> hintStorage;

    LarvaeSession()
        : frontend(&files, &configs, options())
    {
        files.open = &open;
        configs.defaultConfig.mode = Luau::Mode::Nonstrict;
        applyRequiredFlags();

        Luau::registerBuiltinGlobals(frontend, frontend.globals, false);
        Luau::registerBuiltinGlobals(frontend, frontend.globalsForAutocomplete, true);
        Luau::freeze(frontend.globals.globalTypes);
        Luau::freeze(frontend.globalsForAutocomplete.globalTypes);
    }
};

extern "C" {

void larvae_enable_all_flags(void)
{
    enableAllFlags();
}

void larvae_apply_required_flags(void)
{
    applyRequiredFlags();
}

/*
One flag by name, from the text a project wrote.

Luau keeps a boolean list and an integer list, so the name decides which one
is asked. An unknown name is reported rather than ignored: a flag that Luau
renamed is a setting that silently stopped working, and the user is the only
one who can fix it.
*/
int larvae_set_flag(const char* name, const char* value)
{
    for (Luau::FValue<bool>* it = Luau::FValue<bool>::list; it; it = it->next)
    {
        if (strcmp(it->name, name) != 0)
            continue;

        if (strcmp(value, "true") == 0 || strcmp(value, "True") == 0)
            it->value = true;
        else if (strcmp(value, "false") == 0 || strcmp(value, "False") == 0)
            it->value = false;
        else
            return 2;

        return 0;
    }

    for (Luau::FValue<int>* it = Luau::FValue<int>::list; it; it = it->next)
    {
        if (strcmp(it->name, name) != 0)
            continue;

        char* end = nullptr;
        const long parsed = strtol(value, &end, 10);

        if (end == value || *end != '\0')
            return 2;

        it->value = static_cast<int>(parsed);

        return 0;
    }

    return 1;
}

LarvaeSession* larvae_session_new(void)
{
    return new LarvaeSession();
}

void larvae_session_free(LarvaeSession* s)
{
    delete s;
}

void larvae_set_resolver(LarvaeSession* s, void* userdata, larvae_resolve_fn resolve, larvae_load_fn load)
{
    s->files.userdata = userdata;
    s->files.resolve = resolve;
    s->files.load = load;
}

/*
Load one declaration file into the global scope.

The globals are frozen after the built in load, so they are thawed for this
and frozen again. The autocomplete globals take the same text, because a
completion list and a type check that disagree about what exists is worse
than either being wrong on its own.

Whether the big Roblox file loads at all is decided in `applyRequiredFlags`,
not here.
*/
int larvae_set_definitions(LarvaeSession* s, const char* name, const char* source)
{
    Luau::unfreeze(s->frontend.globals.globalTypes);
    Luau::unfreeze(s->frontend.globalsForAutocomplete.globalTypes);

    Luau::LoadDefinitionFileResult result = s->frontend.loadDefinitionFile(
        s->frontend.globals, s->frontend.globals.globalScope, source, name, false, false);

    Luau::LoadDefinitionFileResult forAutocomplete = s->frontend.loadDefinitionFile(
        s->frontend.globalsForAutocomplete, s->frontend.globalsForAutocomplete.globalScope, source, name, false, true);

    Luau::freeze(s->frontend.globals.globalTypes);
    Luau::freeze(s->frontend.globalsForAutocomplete.globalTypes);

    if (!result.success && getenv("LARVAE_DEFS_DEBUG"))
    {
        for (const auto& e : result.parseResult.errors)
            fprintf(stderr, "defs %s:%d: %s\n", name, e.getLocation().begin.line + 1,
                    e.getMessage().c_str());

        if (result.parseResult.errors.empty())
        {
            if (result.module)
            {
                int shown = 0;

                for (const auto& e : result.module->errors)
                {
                    fprintf(stderr, "defs %s:%d: %s\n", name, e.location.begin.line + 1,
                            Luau::toString(e).c_str());

                    if (++shown >= 5)
                        break;
                }

                fprintf(stderr, "defs %s: %zu type error(s)\n", name, result.module->errors.size());
            }
            else
            {
                fprintf(stderr, "defs %s: no module came back\n", name);
            }
        }
    }

    return result.success && forAutocomplete.success ? 0 : 1;
}

void larvae_open(LarvaeSession* s, const char* path, const char* text)
{
    s->open[path] = text;
    s->frontend.markDirty(path);
}

void larvae_invalidate(LarvaeSession* s, const char* path)
{
    std::vector<Luau::ModuleName> dependents;
    s->frontend.markDirty(path, &dependents);

    for (const auto& name : dependents)
        s->frontend.markDirty(name);
}

size_t larvae_check(LarvaeSession* s, const char* path, LarvaeDiag* out, size_t cap)
{
    Luau::CheckResult result;

    try
    {
        result = s->frontend.check(path);
    }
    catch (const std::exception&)
    {
        return 0;
    }

    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    LineIndex lines(it->second);

    s->diagStorage.clear();
    // Reserved up front: a push past capacity moves the strings, and the
    // pointers already written into `out` would dangle.
    s->diagStorage.reserve(cap);
    size_t n = 0;

    for (const Luau::TypeError& error : result.errors)
    {
        if (error.moduleName != path)
            continue;

        if (n >= cap)
            break;

        s->diagStorage.push_back(Luau::toString(error));

        out[n].start = lines.byteOf(error.location.begin, it->second);
        out[n].end = lines.byteOf(error.location.end, it->second);
        out[n].severity = 1;
        out[n].message = s->diagStorage.back().c_str();
        ++n;
    }

    return n;
}

/*
The hover card, in the shape a reader expects to see.

Three things decide whether a hover is useful, and the first cut of this got
all three wrong.

It asked `findTypeAtPosition`, which answers for an expression. A local's
declaration is not an expression, so hovering the name a reader just wrote
gave nothing at all: `local total = add(1, 2)` had a type and would not show
it. `findExprOrLocalAtPosition` answers for both, and a local's type comes
from the scope that holds it.

It printed a function as its type, `(number, number) -> number`. A reader
wants the signature they wrote, with the name and the argument names, and
Luau renders that itself through `toStringNamedFunction`.

It printed a local as a bare type. A card that reads `local total: number`
says what the line is as well as what it holds, which is what luau-lsp shows
and what a reader is looking for.

The options match luau-lsp's, because the two servers should not disagree
about how one type reads.
*/
const char* larvae_hover(LarvaeSession* s, const char* path, uint32_t byte, int show_table_kinds)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return nullptr;

    try
    {
        s->frontend.check(path);
    }
    catch (const std::exception&)
    {
        return nullptr;
    }

    Luau::ModulePtr module = s->frontend.moduleResolver.getModule(path);
    const Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source)
        return nullptr;

    LineIndex lines(it->second);
    Luau::Position position = lines.positionOf(byte);

    Luau::ScopePtr scope = Luau::findScopeAtPosition(*module, position);
    Luau::ExprOrLocal found = Luau::findExprOrLocalAtPosition(*source, position);

    std::optional<Luau::TypeId> type;
    std::string aliasName;

    /*
    A type name answers with what it stands for.

    `type Point = { x: number }` and every later `Point` both read as the
    alias, so hovering either shows the shape the name hides. The type
    namespace is separate from the value namespace, so the scope is asked a
    different question here.
    */
    /*
    Types are asked for outright. The walk leaves them out by default, so a
    `Point` in `local p: Point` was never reached and the reference hovered
    as nothing while its declaration hovered fine.
    */
    std::vector<Luau::AstNode*> ancestry =
        Luau::findAstAncestryOfPosition(*source, position, /* includeTypes = */ true);

    if (scope && !ancestry.empty())
    {
        Luau::AstNode* node = ancestry.back();

        if (auto ref = node->as<Luau::AstTypeReference>())
        {
            std::optional<Luau::TypeFun> fun = ref->prefix
                ? scope->lookupImportedType(ref->prefix->value, ref->name.value)
                : scope->lookupType(ref->name.value);

            if (fun)
            {
                aliasName = ref->prefix
                    ? std::string(ref->prefix->value) + "." + ref->name.value
                    : std::string(ref->name.value);
                type = fun->type;
            }
        }
        else
        {
            // The declaration itself, which is one node above the name.
            for (auto up = ancestry.rbegin(); up != ancestry.rend(); ++up)
            {
                auto alias = (*up)->as<Luau::AstStatTypeAlias>();

                if (!alias || !alias->nameLocation.containsClosed(position))
                    continue;

                if (auto fun = scope->lookupType(alias->name.value))
                {
                    aliasName = alias->name.value;
                    type = fun->type;
                }

                break;
            }
        }
    }

    // A local is not an expression, so the scope answers for it.
    if (!type)
    if (Luau::AstLocal* local = found.getLocal())
    {
        if (scope)
            type = scope->lookup(local);
    }

    if (!type)
        type = Luau::findTypeAtPosition(*module, *source, position);

    if (!type)
        return nullptr;

    Luau::ToStringOptions opts;
    opts.exhaustive = true;
    opts.useLineBreaks = true;
    opts.functionTypeArguments = true;
    opts.hideNamedFunctionTypeParameters = false;
    opts.scope = scope;

    /*
    `{| x: number |}` says the table is sealed, which matters to somebody
    writing a type and to nobody reading one. luau-lsp hides it by default
    for that reason, and a project that wants it turns it back on.
    */
    opts.hideTableKind = show_table_kinds == 0;

    Luau::TypeId followed = Luau::follow(*type);

    /*
    A function shows its signature. The name comes from whichever half of
    the answer carries one, and a function with no name still renders, so an
    anonymous one is not left blank.
    */
    if (const Luau::FunctionType* ftv = Luau::get<Luau::FunctionType>(followed))
    {
        std::string name;

        if (Luau::AstLocal* local = found.getLocal())
            name = local->name.value;
        else if (Luau::AstExpr* expr = found.getExpr())
        {
            if (auto global = expr->as<Luau::AstExprGlobal>())
                name = global->name.value;
            else if (auto localExpr = expr->as<Luau::AstExprLocal>())
                name = localExpr->local->name.value;
            else if (auto index = expr->as<Luau::AstExprIndexName>())
                name = index->index.value;
        }

        /*
        The `function` keyword goes in front, because the card should read
        like the line the author would write. Luau renders the rest.
        */
        s->hoverStorage = name.empty()
            ? "function" + Luau::toStringNamedFunction("", *ftv, opts)
            : "function " + Luau::toStringNamedFunction(name, *ftv, opts);

        return s->hoverStorage.c_str();
    }

    std::string text = Luau::toString(followed, opts);

    // A type name says it is a type, and what it stands for.
    if (!aliasName.empty())
    {
        s->hoverStorage = "type " + aliasName + " = " + text;

        return s->hoverStorage.c_str();
    }

    // A local says what the line is, as well as what it holds.
    if (Luau::AstLocal* local = found.getLocal())
    {
        s->hoverStorage = "local " + std::string(local->name.value) + ": " + text;

        return s->hoverStorage.c_str();
    }

    if (Luau::AstExpr* expr = found.getExpr())
    {
        if (auto localExpr = expr->as<Luau::AstExprLocal>())
        {
            s->hoverStorage = "local " + std::string(localExpr->local->name.value) + ": " + text;

            return s->hoverStorage.c_str();
        }
    }

    s->hoverStorage = text;

    return s->hoverStorage.c_str();
}

/*
Fill a location from a module name and a Luau span.

The path string lives on the session, which is the same rule every other
string here follows: it stays valid until the next call and Rust copies
what it keeps.
*/
static int fillLocation(LarvaeSession* s, const std::string& module, const Luau::Location& at, LarvaeLocation* out)
{
    s->locationStorage = module;

    out->path = s->locationStorage.c_str();
    out->start_line = at.begin.line;
    out->start_character = at.begin.column;
    out->end_line = at.end.line;
    out->end_character = at.end.column;

    return 1;
}

/*
Where the name at a position is declared.

Three shapes answer, and they cover what a reader clicks. A local resolves
to its own declaration, which the AST carries outright. A require resolves
to the module it names, because a click on the string is a click on the
file. Everything else asks the type: a function or a table field carries the
location it was defined at, which is how a click on a method reaches the
module that wrote it.

A name with no answer returns 0 rather than a guess. A wrong jump costs a
reader more than no jump, because they have to work out where they landed.
*/
int larvae_definition(LarvaeSession* s, const char* path, uint32_t byte, LarvaeLocation* out)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    try
    {
        s->frontend.check(path);
    }
    catch (const std::exception&)
    {
        return 0;
    }

    Luau::ModulePtr module = s->frontend.moduleResolver.getModule(path);
    const Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source)
        return 0;

    LineIndex lines(it->second);
    Luau::Position position = lines.positionOf(byte);

    std::vector<Luau::AstNode*> ancestry = Luau::findAstAncestryOfPosition(*source, position);
    if (ancestry.empty())
        return 0;

    Luau::AstNode* node = ancestry.back();

    // A local names its own declaration, and the AST already holds it.
    if (auto* local = node->as<Luau::AstExprLocal>())
        return fillLocation(s, path, local->local->location, out);

    /*
    A click inside a require string opens the module it names. The resolver
    answered that question once already, and the module map holds the
    answer, so nothing here re-resolves a path.
    */
    if (auto* text = node->as<Luau::AstExprConstantString>())
    {
        for (Luau::AstNode* up : ancestry)
        {
            auto* call = up->as<Luau::AstExprCall>();
            if (!call)
                continue;

            auto* callee = call->func->as<Luau::AstExprGlobal>();
            if (!callee || callee->name != "require")
                continue;

            if (!s->files.resolve)
                return 0;

            std::string spec(text->value.data, text->value.size);
            const char* target = s->files.resolve(s->files.userdata, path, spec.c_str());
            if (!target)
                return 0;

            return fillLocation(s, target, Luau::Location{}, out);
        }
    }

    /*
    Everything else asks the type where it came from. A function type
    carries its definition location, which reaches a method or a field
    declared in another module.
    */
    std::optional<Luau::TypeId> type = Luau::findTypeAtPosition(*module, *source, position);
    if (!type)
        return 0;

    if (auto* fn = Luau::get<Luau::FunctionType>(Luau::follow(*type)))
    {
        if (fn->definition)
        {
            // The module name is absent for a function this file declares.
            const std::string& where = fn->definition->definitionModuleName
                ? *fn->definition->definitionModuleName
                : std::string(path);

            return fillLocation(s, where, fn->definition->definitionLocation, out);
        }
    }

    return 0;
}

/*
Where the type of the name at a position is declared.

This is the other jump a reader wants: not the value, the shape of it. A
table type carries the location it was defined at, and a class type carries
its own.
*/
int larvae_type_definition(LarvaeSession* s, const char* path, uint32_t byte, LarvaeLocation* out)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    try
    {
        s->frontend.check(path);
    }
    catch (const std::exception&)
    {
        return 0;
    }

    Luau::ModulePtr module = s->frontend.moduleResolver.getModule(path);
    const Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source)
        return 0;

    LineIndex lines(it->second);
    std::optional<Luau::TypeId> type = Luau::findTypeAtPosition(*module, *source, lines.positionOf(byte));
    if (!type)
        return 0;

    Luau::TypeId followed = Luau::follow(*type);

    if (auto* table = Luau::get<Luau::TableType>(followed))
    {
        // An empty name means the table was written here, not imported.
        const std::string& where =
            table->definitionModuleName.empty() ? std::string(path) : table->definitionModuleName;

        if (table->definitionLocation != Luau::Location{})
            return fillLocation(s, where, table->definitionLocation, out);
    }

    return 0;
}


/*
The signature of the call that encloses a position.

The walk goes outward from the position to the nearest call, because a
caret inside an argument is still inside the call that takes it. Luau gives
the callee's type, and a function type carries its argument pack and the
names the author wrote, so the label is built from the type and not from the
source text. A callee with no function type has no signature to show.

The active parameter counts the commas the caret has passed. That is what
the editor bolds, and it is why an incomplete call still answers: the author
is mid-typing, which is exactly when the help is wanted.
*/
int larvae_signature_help(
    LarvaeSession* s, const char* path, uint32_t byte,
    LarvaeSignature* sig, LarvaeParameter* out, size_t cap)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    try
    {
        s->frontend.check(path);
    }
    catch (const std::exception&)
    {
        return 0;
    }

    Luau::ModulePtr module = s->frontend.moduleResolver.getModule(path);
    const Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source)
        return 0;

    LineIndex lines(it->second);
    Luau::Position position = lines.positionOf(byte);

    std::vector<Luau::AstNode*> ancestry = Luau::findAstAncestryOfPosition(*source, position);

    Luau::AstExprCall* call = nullptr;
    for (auto node = ancestry.rbegin(); node != ancestry.rend(); ++node)
    {
        if (auto* found = (*node)->as<Luau::AstExprCall>())
        {
            call = found;
            break;
        }
    }

    if (!call)
        return 0;

    auto* callee = module->astTypes.find(call->func);
    if (!callee)
        return 0;

    const Luau::FunctionType* fn = Luau::get<Luau::FunctionType>(Luau::follow(*callee));
    if (!fn)
        return 0;

    Luau::ToStringOptions opts;
    opts.exhaustive = false;
    opts.maxTypeLength = 200;

    auto [args, tail] = Luau::flatten(fn->argTypes);

    s->parameterStorage.clear();

    /*
    A method call passes the receiver as the first argument, and the author
    did not write it. To show it would put the caret on the wrong parameter
    for every call.
    */
    size_t first = (call->self && !args.empty()) ? 1 : 0;

    for (size_t i = first; i < args.size(); ++i)
    {
        std::string label;

        if (i < fn->argNames.size() && fn->argNames[i])
            label = fn->argNames[i]->name + ": ";

        label += Luau::toString(args[i], opts);
        s->parameterStorage.push_back(label);
    }

    if (tail)
        s->parameterStorage.push_back("...");

    std::string label = "(";
    for (size_t i = 0; i < s->parameterStorage.size(); ++i)
    {
        if (i)
            label += ", ";
        label += s->parameterStorage[i];
    }
    label += "): " + Luau::toString(fn->retTypes, opts);

    s->signatureStorage = label;

    // The caret has passed one argument for each argument that ends before it.
    uint32_t active = 0;
    for (Luau::AstExpr* arg : call->args)
    {
        if (arg->location.end < position)
            active++;
    }

    sig->label = s->signatureStorage.c_str();
    sig->active = active;
    sig->count = s->parameterStorage.size();

    size_t n = s->parameterStorage.size() < cap ? s->parameterStorage.size() : cap;
    for (size_t i = 0; i < n; ++i)
        out[i].label = s->parameterStorage[i].c_str();

    return 1;
}

/*
Collect the inlay hints of one module.

Two kinds, and both answer the same question: what is the type the author
did not write. A local with no annotation gets its inferred type after the
name. A function parameter and a return type get the same, which is what
makes an unannotated codebase readable without changing it.

The walk visits the AST rather than the type graph, because a hint belongs
at a place in the text, and only the AST knows where the author wrote a
name. A binding that carries an annotation is skipped: the type is on the
screen already, and to repeat it is noise.
*/
struct HintCollector : Luau::AstVisitor
{
    LarvaeSession* session;
    Luau::ModulePtr module;
    std::vector<std::pair<Luau::Position, std::pair<std::string, uint8_t>>> found;

    Luau::ToStringOptions opts;

    HintCollector(LarvaeSession* s, Luau::ModulePtr m)
        : session(s)
        , module(std::move(m))
    {
        opts.exhaustive = false;
        opts.maxTypeLength = 60;
    }

    /*
    The rendered type, or nothing when the hint would not help.

    Luau keys its type map by expression, and a local is not an expression,
    so the type comes from what the local was given: the value in a `local`
    statement, and the argument pack of the enclosing function for a
    parameter. That is the same answer by a different road.
    */
    std::optional<std::string> render(Luau::TypeId type)
    {
        std::string text = Luau::toString(Luau::follow(type), opts);

        // A type nobody can act on is not worth the space it takes.
        if (text.empty() || text == "any" || text == "*error-type*")
            return std::nullopt;

        return text;
    }

    bool visit(Luau::AstStatLocal* node) override
    {
        for (size_t i = 0; i < node->vars.size; ++i)
        {
            Luau::AstLocal* local = node->vars.data[i];

            // An annotation puts the type on screen already.
            if (!local || local->annotation)
                continue;

            if (i >= node->values.size)
                break;

            auto* type = module->astTypes.find(node->values.data[i]);
            if (!type)
                continue;

            if (auto text = render(*type))
                found.push_back({local->location.end, {": " + *text, 1}});
        }

        return true;
    }

    bool visit(Luau::AstExprFunction* node) override
    {
        auto* self = module->astTypes.find(node);
        if (!self)
            return true;

        const Luau::FunctionType* fn = Luau::get<Luau::FunctionType>(Luau::follow(*self));
        if (!fn)
            return true;

        auto [args, tail] = Luau::flatten(fn->argTypes);
        (void)tail;

        // A method takes the receiver first, and the author did not write it.
        size_t offset = node->self ? 1 : 0;

        for (size_t i = 0; i < node->args.size; ++i)
        {
            Luau::AstLocal* arg = node->args.data[i];

            if (!arg || arg->annotation)
                continue;

            size_t index = i + offset;
            if (index >= args.size())
                break;

            if (auto text = render(args[index]))
                found.push_back({arg->location.end, {": " + *text, 1}});
        }

        return true;
    }
};

size_t larvae_inlay_hints(LarvaeSession* s, const char* path, LarvaeHint* out, size_t cap)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    try
    {
        s->frontend.check(path);
    }
    catch (const std::exception&)
    {
        return 0;
    }

    Luau::ModulePtr module = s->frontend.moduleResolver.getModule(path);
    const Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source || !source->root)
        return 0;

    HintCollector collector(s, module);
    source->root->visit(&collector);

    s->hintStorage.clear();
    s->hintStorage.reserve(collector.found.size());

    for (auto& entry : collector.found)
        s->hintStorage.push_back(entry.second.first);

    size_t n = collector.found.size() < cap ? collector.found.size() : cap;

    for (size_t i = 0; i < n; ++i)
    {
        out[i].line = collector.found[i].first.line;
        out[i].character = collector.found[i].first.column;
        out[i].label = s->hintStorage[i].c_str();
        out[i].kind = collector.found[i].second.second;
    }

    return collector.found.size();
}

size_t larvae_completions(LarvaeSession* s, const char* path, uint32_t byte, LarvaeCompletion* out, size_t cap)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    LineIndex lines(it->second);
    Luau::Position position = lines.positionOf(byte);

    Luau::AutocompleteResult result;

    try
    {
        /*
        Autocomplete reads the module that the autocomplete typechecker
        built, which exists only after a check that asked for it.
        */
        Luau::FrontendOptions forAutocomplete = options();
        forAutocomplete.forAutocomplete = true;
        s->frontend.check(path, forAutocomplete);

        result = Luau::autocomplete(s->frontend, path, position,
            [](std::string, std::optional<const Luau::ExternType*>,
                std::optional<std::string>) -> std::optional<Luau::AutocompleteEntryMap> {
                return std::nullopt;
            });
    }
    catch (const std::exception&)
    {
        return 0;
    }

    s->completionStorage.clear();
    // Reserved for the same reason as the diagnostics: no reallocation
    // after the first pointer is handed out.
    s->completionStorage.reserve(cap);
    size_t n = 0;

    for (const auto& [label, entry] : result.entryMap)
    {
        if (n >= cap)
            break;

        s->completionStorage.push_back(label);

        uint8_t kind = 12; /* Value */
        switch (entry.kind)
        {
        case Luau::AutocompleteEntryKind::Property:
            kind = 5; /* Field */
            break;
        case Luau::AutocompleteEntryKind::Keyword:
            kind = 14; /* Keyword */
            break;
        case Luau::AutocompleteEntryKind::Module:
            kind = 9; /* Module */
            break;
        default:
            break;
        }

        if (entry.type && Luau::get<Luau::FunctionType>(Luau::follow(*entry.type)))
            kind = 3; /* Function */

        out[n].label = s->completionStorage.back().c_str();
        out[n].kind = kind;
        ++n;
    }

    return n;
}

} // extern "C"
