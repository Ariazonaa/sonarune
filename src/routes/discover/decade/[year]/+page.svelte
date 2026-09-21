<script lang="ts">
  import { page } from "$app/state";
  import { m } from "$lib/paraglide/messages";
  import PaginatedDiscoveryAlbums from "$lib/components/PaginatedDiscoveryAlbums.svelte";

  const startYear = $derived(Number.parseInt(page.params.year ?? "0", 10));
</script>

<PaginatedDiscoveryAlbums
  title={m.discover_decade_title({ year: Number.isFinite(startYear) ? startYear : 0 })}
  kind="decade"
  {startYear}
/>
