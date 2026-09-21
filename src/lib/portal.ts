import type { Attachment } from "svelte/attachments";

/**
 * Render an element in `<body>` instead of where it is declared.
 *
 * Every Liquid Glass surface (`main`, the side panels, the player bar) has a
 * `backdrop-filter`, and that makes it the containing block for
 * `position: fixed`. A menu or modal declared inside a page would be offset by
 * the page's own position, clipped by its scroll box and scrolled out of view
 * with it — measured in Chromium 152: a fixed menu at 10/10 inside a scrolled
 * glass panel landed at 260/-430.
 *
 * Never attach this to a top-level node of a block (the root of an `{#if}`
 * or of a component): Svelte removes a block by walking its nodes from first
 * to last sibling, and a boundary node moved elsewhere breaks that walk. Wrap
 * the portaled element in a plain element that stays behind.
 */
export const portal: Attachment<HTMLElement> = (node) => {
  document.body.appendChild(node);
  return () => node.remove();
};
