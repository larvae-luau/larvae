/*!
use_get_service: `game.Players` becomes `game:GetService("Players")`. The
property form breaks when a service has a new name or does not exist yet.
GetService always returns the live service.

The rule has no scope tracking yet. Thus a file that binds the name
`game` anywhere turns the rule off for that whole file. A shadow of
`game` is very rare, and a skip of the file is better than a rewrite of
another table.
*/

use crate::requires::resolve::lua_quote;
use crate::rules::engine::{self, Edit, Flow, RuleCtx, Visit};
use crate::rules::native::name_text;
use crate::syntax::ast::{Expr, IndexKey, Stmt};

/*
Services that authors reference in code. The list is curated and not
generated. An unknown name stays a plain property access.
*/
const SERVICES: &[&str] = &[
    "Players",
    "Workspace",
    "Lighting",
    "ReplicatedStorage",
    "ReplicatedFirst",
    "ServerScriptService",
    "ServerStorage",
    "StarterGui",
    "StarterPack",
    "StarterPlayer",
    "Teams",
    "SoundService",
    "Chat",
    "TextService",
    "TextChatService",
    "RunService",
    "TweenService",
    "UserInputService",
    "ContextActionService",
    "HttpService",
    "DataStoreService",
    "MemoryStoreService",
    "MessagingService",
    "MarketplaceService",
    "TeleportService",
    "BadgeService",
    "PolicyService",
    "SocialService",
    "GroupService",
    "PathfindingService",
    "PhysicsService",
    "CollectionService",
    "ProximityPromptService",
    "GuiService",
    "VRService",
    "HapticService",
    "LocalizationService",
    "LogService",
    "ScriptContext",
    "Debris",
    "InsertService",
    "AssetService",
    "VoiceChatService",
];

pub fn apply(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    if binds_game(ctx) {
        return;
    }

    let mut rewriter = Rewriter {
        ctx,
        edits,
        targets: Vec::new(),
    };

    engine::walk_chunk(ctx.chunk, &mut rewriter);
}

/// True when the file binds the name `game` anywhere. Example: a local or a param.
fn binds_game(ctx: &RuleCtx) -> bool {
    struct Shadow<'a, 'src> {
        ctx: &'a RuleCtx<'src>,
        found: bool,
    }

    impl Shadow<'_, '_> {
        fn mark(&mut self, span: crate::syntax::ast::TokSpan) {
            if name_text(self.ctx, span) == "game" {
                self.found = true;
            }
        }
    }

    impl Visit for Shadow<'_, '_> {
        fn stmt(&mut self, stmt: &Stmt) -> Flow {
            match stmt {
                Stmt::Local(n) => {
                    for binding in &n.names {
                        self.mark(binding.name);
                    }
                }

                Stmt::LocalFunction(n) => {
                    self.mark(n.name);

                    for param in &n.body.params {
                        self.mark(param.name);
                    }
                }

                Stmt::Function(n) => {
                    for param in &n.body.params {
                        self.mark(param.name);
                    }
                }

                Stmt::NumericFor(n) => self.mark(n.var.name),

                Stmt::GenericFor(n) => {
                    for var in &n.vars {
                        self.mark(var.name);
                    }
                }

                _ => {}
            }

            Flow::Next
        }

        fn expr(&mut self, expr: &Expr) -> Flow {
            if let Expr::Function { body, .. } = expr {
                for param in &body.params {
                    self.mark(param.name);
                }
            }

            Flow::Next
        }
    }

    let mut shadow = Shadow { ctx, found: false };
    engine::walk_chunk(ctx.chunk, &mut shadow);

    shadow.found
}

struct Rewriter<'a, 'src> {
    ctx: &'a RuleCtx<'src>,
    edits: &'a mut Vec<Edit>,
    /// The byte starts of assignment targets. `game.Players = x` is not an expression.
    targets: Vec<u32>,
}

impl Visit for Rewriter<'_, '_> {
    fn stmt(&mut self, stmt: &Stmt) -> Flow {
        if let Stmt::Assign(assign) = stmt {
            for target in &assign.targets {
                if service_of(self.ctx, target).is_some() {
                    self.targets.push(self.ctx.bytes(target.span()).0);
                }
            }
        }

        Flow::Next
    }

    fn expr(&mut self, expr: &Expr) -> Flow {
        let Some(service) = service_of(self.ctx, expr) else {
            return Flow::Next;
        };

        let (start, _) = self.ctx.bytes(expr.span());

        if self.targets.contains(&start) {
            return Flow::Next;
        }

        let call = format!("game:GetService({})", lua_quote(service, self.ctx.quote));
        self.ctx.replace(expr.span(), call, self.edits);

        Flow::Next
    }
}

/// The service name when this expression is exactly `game.SomeService`.
fn service_of<'src>(ctx: &RuleCtx<'src>, expr: &Expr) -> Option<&'src str> {
    let Expr::Index {
        object,
        key: IndexKey::Field(field),
        ..
    } = expr
    else {
        return None;
    };

    if !matches!(&**object, Expr::Name(n) if name_text(ctx,*n) == "game") {
        return None;
    }

    let name = name_text(ctx, *field);

    SERVICES.contains(&name).then_some(name)
}

#[cfg(test)]
mod tests {
    use crate::rules::native::test_support::run;

    const ON: &str = "use_get_service = true";

    #[test]
    fn rewrites_service_properties() {
        assert_eq!(
            run(ON, "local rs = game.ReplicatedStorage\n"),
            "local rs = game:GetService(\"ReplicatedStorage\")\n"
        );
        // In a deeper chain, the rule rewrites the service and keeps the rest.
        assert_eq!(
            run(ON, "local p = game.Players.LocalPlayer\n"),
            "local p = game:GetService(\"Players\").LocalPlayer\n"
        );
        // Call and argument positions also rewrite.
        assert_eq!(
            run(ON, "game.Debris:AddItem(part, 1)\n"),
            "game:GetService(\"Debris\"):AddItem(part, 1)\n"
        );
        assert_eq!(
            run(ON, "f(game.RunService, game.Workspace)\n"),
            "f(game:GetService(\"RunService\"), game:GetService(\"Workspace\"))\n"
        );
    }

    #[test]
    fn leaves_non_services_alone() {
        // Not a service name.
        let child = "local x = game.SomeFolder\n";
        assert_eq!(run(ON, child), child);
        // The receiver is not the global game.
        let other = "local x = other.Players\n";
        assert_eq!(run(ON, other), other);
        // A computed index. The key is not a name.
        let computed = "local x = game[\"Players\"]\n";
        assert_eq!(run(ON, computed), computed);
        // An assignment target would become a call on the left hand side.
        let target = "game.Lighting = nil\n";
        assert_eq!(run(ON, target), target);
        // For a property under a service, the rule still rewrites only the service.
        assert_eq!(
            run(ON, "game.Lighting.Ambient = c\n"),
            "game:GetService(\"Lighting\").Ambient = c\n"
        );
    }

    #[test]
    fn a_shadowed_game_turns_the_file_off() {
        // The local wins over the global. This game is a different table.
        let local = "local game = fake\nreturn game.Players\n";
        assert_eq!(run(ON, local), local);
        // The same applies to a parameter, anywhere in the file.
        let param = "local function f(game)\n    return game.Players\nend\nreturn game.Workspace\n";
        assert_eq!(run(ON, param), param);
    }
}
