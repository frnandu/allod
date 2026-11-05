import { IMPOSSIBLE, VersionInfo } from '@start9labs/start-sdk'

export const current = VersionInfo.of({
  version: '0.1.1:3',
  releaseNotes: {
    en_US:
      'Secure one-click first-run setup with a launchable Open setup action',
    es_ES: 'Configuración inicial segura con la acción Abrir configuración',
    de_DE: 'Sichere Ersteinrichtung mit der Aktion Einrichtung öffnen',
    pl_PL: 'Bezpieczna pierwsza konfiguracja przez akcję Otwórz konfigurację',
    fr_FR:
      "Configuration initiale sécurisée avec l'action Ouvrir la configuration",
  },
  migrations: {
    up: async () => {},
    down: IMPOSSIBLE,
  },
})
