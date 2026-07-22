let settingsV2PresentationActive = false;

export function configureSettingsV2Presentation(options: {
  readonly active: boolean;
}): void {
  settingsV2PresentationActive = options.active;
}

export function settingsV2Active(): boolean {
  return settingsV2PresentationActive;
}
