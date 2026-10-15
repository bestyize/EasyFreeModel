import type { Api } from '../types'
import { providersMock } from './providers'
import { oauthMock } from './oauth'
import { apiKeysMock } from './apiKeys'
import { usageMock } from './usage'
import { quotaMock } from './quota'
import { systemMock } from './system'
import { mediaMock } from './media'
import { proxyMock } from './proxy'
import { integrationsMock } from './integrations'

export const mockApi: Api = {
  ...providersMock,
  ...oauthMock,
  ...apiKeysMock,
  ...usageMock,
  ...quotaMock,
  ...systemMock,
  ...mediaMock,
  ...proxyMock,
  ...integrationsMock,
}
