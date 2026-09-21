<script lang="ts">
  import { m } from "$lib/paraglide/messages";
  import PaginatedDiscoveryAlbums from "$lib/components/PaginatedDiscoveryAlbums.svelte";
</script>

<PaginatedDiscoveryAlbums title={m.discover_random_albums()} kind="random" />
