@id("captain-links-cards")
permit (
  principal,
  action == Matos::Action::"card.update",
  resource
)
when {
  context.actor_kind == "captain" && context has actor &&
  context.organization == resource.organization &&
  context.intent == "link" &&
  !(context has column)
};
