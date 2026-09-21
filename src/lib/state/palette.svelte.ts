/** Command palette (Ctrl+K) open state — shared by shortcuts + layout. */
class PaletteStore {
  open = $state(false);

  show = () => {
    this.open = true;
  };
  close = () => {
    this.open = false;
  };
  toggle = () => {
    this.open = !this.open;
  };
}

export const palette = new PaletteStore();
