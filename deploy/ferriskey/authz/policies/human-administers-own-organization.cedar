@id("human-administers-own-organization")
permit (
  principal,
  action in [
    Matos::Action::"provider.manage",
    Matos::Action::"models.assign",
    Matos::Action::"github.manage",
    Matos::Action::"integration.manage",
    Matos::Action::"budget.manage"
  ],
  resource
)
when {
  context.actor_kind == "human" && !(context has actor) &&
  context.organization == resource.organization &&
  context.is_admin
};
