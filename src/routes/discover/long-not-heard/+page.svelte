<script lang="ts">
  import { m } from "$lib/paraglide/messages";
  import PaginatedDiscoveryAlbums from "$lib/components/PaginatedDiscoveryAlbums.svelte";
</script>

<PaginatedDiscoveryAlbums title={m.discover_long_not_heard()} kind="long" />
