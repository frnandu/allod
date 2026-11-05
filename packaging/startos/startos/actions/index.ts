import { sdk } from '../sdk'
import { addOwner } from './addOwner'
import { confirmRestore } from './confirmRestore'
import { connectionInfo } from './connectionInfo'
import { skipBackupCheck } from './skipBackupCheck'
import { openSetup } from './openSetup'

export const actions = sdk.Actions.of()
  .addAction(connectionInfo)
  .addAction(openSetup)
  .addAction(addOwner)
  .addAction(confirmRestore)
  .addAction(skipBackupCheck)
