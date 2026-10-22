<!-- Test-only host for rowKeys.test.ts: a clickable track row like the album
     page's, with a link and a button inside it. -->
<script lang="ts">
  import { activatesRow } from "#lib/rowKeys.js";

  let { onplay }: { onplay: () => void } = $props();
</script>

<table>
  <tbody>
    <tr
      id="row"
      tabindex="0"
      role="button"
      onclick={onplay}
      onkeydown={(e) => {
        if (activatesRow(e)) {
          e.preventDefault();
          onplay();
        }
      }}
    >
      <td>
        <a id="link" href="#artist" onclick={(e) => e.stopPropagation()}>artist</a>
        <button id="heart" onclick={(e) => e.stopPropagation()}>heart</button>
      </td>
    </tr>
  </tbody>
</table>
