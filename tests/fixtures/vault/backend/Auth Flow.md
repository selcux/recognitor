---
title: Auth Flow
type: spec
tags: [security, oauth]
---

# Auth Flow

Handles OAuth2 token refresh and session rotation across backend services.

## Observations
- [fact] Access tokens expire after 15 minutes. #tokens
- [decision] @effective:2026-06-10 Refresh tokens are rotated on every exchange.

## Relations
- part_of [[Security Architecture]]
- supersedes [[Legacy Cookie Auth]] (replaced after the 2026 audit)
- relates_to [[User Session Store]]
