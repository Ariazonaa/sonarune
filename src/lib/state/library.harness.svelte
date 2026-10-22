<!-- Test-only host for library.test.ts: a view that registers a library-change
     handler the way the routes do, during its initialisation. -->
<script lang="ts">
  import { onLibraryChange } from "./library.svelte.js";

  let { onchange }: { onchange: () => void } = $props();

  onLibraryChange(() => onchange());
</script>
