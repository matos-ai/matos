@id("deny-non-human-leaving-proposed")
forbid (
  principal,
  action == Matos::Action::"card.move",
  resource
)
when {
  context.actor_kind != "human" && context.from == "proposed"
};
