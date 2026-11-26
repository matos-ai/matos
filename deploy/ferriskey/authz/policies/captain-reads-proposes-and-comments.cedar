@id("captain-reads-proposes-and-comments")
permit (
  principal,
  action in [
    Matos::Action::"card.create",
    Matos::Action::"card.read",
    Matos::Action::"card.comment",
    Matos::Action::"client.read",
    Matos::Action::"github.read"
  ],
  resource
)
when {
  context.actor_kind == "captain" && context has actor &&
  context.organization == resource.organization
};
