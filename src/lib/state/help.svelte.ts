// Visibility of the keyboard-shortcut help overlay (toggled with "?").
class HelpStore {
  open = $state(false);

  toggle() {
    this.open = !this.open;
  }

  close() {
    this.open = false;
  }
}

export const help = new HelpStore();
