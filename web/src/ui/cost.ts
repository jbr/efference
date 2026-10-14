/** A session's billed cost as the client writes it: the provider's running
 * total, marked `~` when at least one request could not be priced, so the
 * number reads as a floor rather than an exact total. The count of unpriced
 * requests is not written — past the first, "inexact" is the whole of what the
 * reader needs (docs/web-client.md "The top bar"). */
export function costLabel(cost: { usd: number; unpriced: number }): string {
  return `${cost.unpriced > 0 ? "~" : ""}$${cost.usd.toFixed(2)}`;
}
