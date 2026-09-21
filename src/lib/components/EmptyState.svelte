<script lang="ts" module>
  // 24x24 symbol per empty view.
  const ICONS = {
    albums:
      "M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 14.5c-2.49 0-4.5-2.01-4.5-4.5S9.51 7.5 12 7.5s4.5 2.01 4.5 4.5-2.01 4.5-4.5 4.5zm0-5.5c-.55 0-1 .45-1 1s.45 1 1 1 1-.45 1-1-.45-1-1-1z",
    heart:
      "M16.5 3c-1.74 0-3.41.81-4.5 2.09C10.91 3.81 9.24 3 7.5 3 4.42 3 2 5.42 2 8.5c0 3.78 3.4 6.86 8.55 11.54L12 21.35l1.45-1.32C18.6 15.36 22 12.28 22 8.5 22 5.42 19.58 3 16.5 3zm-4.4 15.55l-.1.1-.1-.1C7.14 14.24 4 11.39 4 8.5 4 6.5 5.5 5 7.5 5c1.54 0 3.04.99 3.57 2.36h1.87C13.46 5.99 14.96 5 16.5 5c2 0 3.5 1.5 3.5 3.5 0 2.89-3.14 5.74-7.9 10.05z",
    playlist:
      "M15 6H3v2h12V6zm0 4H3v2h12v-2zM3 16h8v-2H3v2zM17 6v8.18c-.31-.11-.65-.18-1-.18-1.66 0-3 1.34-3 3s1.34 3 3 3 3-1.34 3-3V8h3V6h-5z",
    search:
      "M15.5 14h-.79l-.28-.27A6.47 6.47 0 0 0 16 9.5 6.5 6.5 0 1 0 9.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z",
    stats: "M5 9.2h3V19H5V9.2zM10.6 5h2.8v14h-2.8V5zm5.6 8H19v6h-2.8v-6z",
    discover:
      "M12 10.9c-.61 0-1.1.49-1.1 1.1s.49 1.1 1.1 1.1c.61 0 1.1-.49 1.1-1.1s-.49-1.1-1.1-1.1zM12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm2.19 12.19L6 18l3.81-8.19L18 6l-3.81 8.19z",
    collection:
      "M20 2H8c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zm-8 12.5v-9l6 4.5-6 4.5zM4 6H2v14c0 1.1.9 2 2 2h14v-2H4V6z",
    genre:
      "M21.41 11.58l-9-9C12.05 2.22 11.55 2 11 2H4c-1.1 0-2 .9-2 2v7c0 .55.22 1.05.59 1.42l9 9c.36.36.86.58 1.41.58.55 0 1.05-.22 1.41-.59l7-7c.37-.36.59-.86.59-1.41 0-.55-.23-1.06-.59-1.42zM5.5 7C4.67 7 4 6.33 4 5.5S4.67 4 5.5 4 7 4.67 7 5.5 6.33 7 5.5 7z",
    filter: "M10 18h4v-2h-4v2zM3 6v2h18V6H3zm3 7h12v-2H6v2z",
    home: "M10 20v-6h4v6h5v-8h3L12 3 2 12h3v8z",
    lyrics:
      "M12 2a4 4 0 0 0-4 4v6a4 4 0 0 0 8 0V6a4 4 0 0 0-4-4zm-6 10a6 6 0 0 0 5 5.92V21h2v-3.08A6 6 0 0 0 18 12h-2a4 4 0 0 1-8 0H6z",
  } as const;

  type EmptyIcon = keyof typeof ICONS;

  /** Call to action: a link (`href`) or a button (`onclick`). */
  interface EmptyAction {
    label: string;
    href?: string;
    onclick?: () => void;
  }
</script>

<script lang="ts">
  interface Props {
    icon: EmptyIcon;
    title: string;
    hint?: string;
    action?: EmptyAction;
    /** Smaller, for panels and page sections. */
    compact?: boolean;
  }

  let { icon, title, hint, action, compact = false }: Props = $props();

  const actionClass =
    "mt-1 rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover";
</script>

<div class="flex flex-col items-center text-center {compact ? 'gap-2 px-4 py-6' : 'gap-3 px-6 py-14'}">
  <!-- A small still life in theme tokens: two tilted glass cards behind a
       tinted disc carrying the view's symbol. -->
  <div class="relative {compact ? 'mb-1 h-14 w-14' : 'mb-2 h-24 w-24'}" aria-hidden="true">
    <div class="absolute inset-0 -rotate-12 rounded-xl bg-ink/5 ring-1 ring-edge"></div>
    <div class="absolute inset-0 rotate-6 rounded-xl bg-ink/5 ring-1 ring-edge"></div>
    <div
      class="absolute inset-[18%] flex items-center justify-center rounded-full bg-accent/15 text-accent ring-1 ring-accent/25"
    >
      <svg viewBox="0 0 24 24" class={compact ? "h-5 w-5" : "h-8 w-8"} fill="currentColor">
        <path d={ICONS[icon]} />
      </svg>
    </div>
  </div>
  <p class="max-w-sm font-semibold {compact ? 'text-sm' : 'text-md'}">{title}</p>
  {#if hint}
    <p class="max-w-sm text-sm text-ink-muted">{hint}</p>
  {/if}
  {#if action}
    {#if action.href}
      <a href={action.href} class={actionClass}>{action.label}</a>
    {:else}
      <button class={actionClass} onclick={action.onclick}>{action.label}</button>
    {/if}
  {/if}
</div>
