@id("human-cancels-own-or-any-run-as-admin")
permit (
  principal,
  action == Matos::Action::"run.cancel",
  resource
)
when {
  context.actor_kind == "human" && !(context has actor) &&
  context.organization == resource.organization &&
  (context.initiated || context.is_admin)
};
