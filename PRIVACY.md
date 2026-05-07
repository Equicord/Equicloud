# Privacy Policy

**Effective Date:** September 24, 2025
**Last Updated:** October 27, 2025

## Overview

EquiCloud is a privacy-focused settings backup service that allows users to store and sync their application settings. This privacy policy explains how we collect, use, and protect your data.

## Data We Collect

### Authentication Data
- **Discord User ID**: Collected via Discord OAuth2 to identify and authenticate users
- **Discord Access Token**: Temporarily obtained during authentication and immediately discarded after retrieving your user ID (never stored in our database)
- **User Secret**: A deterministic authentication token generated from your Discord user ID using SHA-256 hashing

### User Settings Data
- **Settings Files**: Application settings and configurations you choose to backup, stored as binary data (up to 60MB by default)
- **Content**: We store your settings as opaque binary blobs without inspection, parsing, or modification
- **Timestamps**:
  - Creation time (`created_at`): When you first saved settings
  - Update time (`updated_at`): Last modification time, used for ETag caching

### What We Do NOT Collect
- ❌ Email addresses
- ❌ Passwords or credentials
- ❌ IP addresses
- ❌ Browser fingerprints or user agents
- ❌ Cookies or tracking data
- ❌ Analytics or behavioral data (beyond optional aggregate metrics)
- ❌ Personal information beyond Discord user ID

## How We Use Your Data

### Authentication & Authorization
- **Identity Verification**: Verify your identity through Discord OAuth2
- **Token Generation**: Generate authentication secrets derived from your Discord user ID
- **Access Control**: Ensure only you can access your settings data
- **Whitelist Enforcement**: If configured, restrict access to approved Discord user IDs only

### Settings Management
- **Storage**: Store your settings as encrypted binary blobs in ScyllaDB
- **Retrieval**: Return your settings when requested
- **Caching**: Use timestamps (ETags) to optimize data transfer
- **Size Validation**: Enforce storage limits to prevent abuse

### System Logging
- **Authentication Events**: Log Discord user IDs when users authenticate (for security auditing)
- **Error Handling**: Log generic error messages without sensitive data
- **Log Level**: Configurable via `RUST_LOG` environment variable (default: info)

**Important**: Server logs may contain your Discord user ID. Log retention depends on server configuration and is typically managed by system administrators.

### Metrics & Analytics (Disabled by Default)
- **Aggregate Statistics**: If enabled (`METRICS_ENABLED=true`), exposes only aggregate user counts:
  - Total users
  - Users active in last 24 hours, 7 days, 30 days
  - Server uptime
- **No Individual Tracking**: Only aggregate counts are collected; no individual user data is exposed
- **Opt-Out**: Metrics endpoint returns 404 when disabled

## Data Storage, Security, and Retention

### Storage Infrastructure
- **Database**: ScyllaDB (Cassandra-compatible distributed database)
- **Data Format**: Settings stored as binary BLOBs (not parsed or inspected)
- **User ID Hashing**: Discord user IDs are hashed using SHA-256 before storage
  - Format: `settings:<first_8_bytes_of_sha256_hex>`
  - Original Discord IDs cannot be recovered from stored hashes
- **Secret Generation**: User authentication secrets derived using SHA-256 hashing
  - Format: First 16 bytes of `SHA256("secret:" + discord_user_id)`

### Security Measures
- **Access Control**: Only authenticated users can access their own data
- **Stateless Authentication**: No server-side session storage; tokens are verified on each request
- **Hashed Identifiers**: Raw Discord IDs never stored in database
- **Opaque Storage**: Settings content is never inspected or modified
- **Size Limits**: 60MB maximum per user (configurable) to prevent abuse
- **CORS Protection**: Configurable allowed origins for cross-origin requests
- **No Cookies**: Application does not use cookies

### Data Retention
- **Settings**: Stored indefinitely until you delete them
- **Deletion Options**:
  - `DELETE /v1/settings`: Remove settings only (keep account)
  - `DELETE /v1`: Remove all data including account
- **Immediate Deletion**: Data is deleted immediately upon request
- **No Backups**: Once deleted, data cannot be recovered
- **Logs**: Retention depends on server configuration (not defined by application)

## Your Rights

You have the right to:
- **Access**: View your stored settings data via `GET /v1/settings`
- **Retrieve**: Download your complete settings backup
- **Modify**: Update your settings at any time via `PUT /v1/settings`
- **Delete Settings**: Remove your settings using `DELETE /v1/settings`
- **Delete Account**: Remove all data including account using `DELETE /v1`
- **Conditional Access**: Use ETags (`If-None-Match`) to optimize data transfer

All operations require authentication via your user secret token.

## Third-Party Services

### Discord OAuth2
- **Purpose**: User authentication only
- **Data Shared with Discord**:
  - OAuth client credentials
  - Authorization code (from OAuth callback)
- **Data Received from Discord**:
  - Access token (temporarily, immediately discarded)
  - Discord user ID (the only data we retain)
- **Scope**: `identify` only (no access to servers, messages, or other Discord data)
- **Token Storage**: Discord access tokens are NEVER stored in our database

### ScyllaDB
- **Purpose**: Database backend for settings storage
- **Deployment**: Self-hosted (not a third-party cloud service)
- **Data Stored**: Hashed user IDs, settings blobs, timestamps

### No Other Third Parties
- ✓ No analytics services (Google Analytics, etc.)
- ✓ No advertising networks
- ✓ No email services
- ✓ No payment processors
- ✓ No CDN for user content
- ✓ No cloud storage providers

## Data Sharing

We do not sell, trade, or share your personal data with third parties except:
- **Legal Obligations**: When required by law or valid legal process
- **Safety & Security**: To protect our rights, safety, or the safety of others
- **Discord OAuth**: Only during authentication flow (as described above)

**We never**:
- Sell user data
- Share data with advertisers
- Provide data to analytics companies
- Transfer data for marketing purposes

## Administrator Controls

Service administrators may:
- **Configure Whitelist**: Restrict access to approved Discord user IDs via `DISCORD_ALLOWED_USER_IDS`
- **Set Size Limits**: Configure maximum backup size via `MAX_BACKUP_SIZE_BYTES`
- **Enable Metrics**: Toggle aggregate metrics endpoint via `METRICS_ENABLED`
- **Access Logs**: View server logs which may contain Discord user IDs for security auditing
- **Database Access**: Query ScyllaDB directly (user IDs are hashed)

Administrators **cannot**:
- Decrypt or reverse user ID hashes without the original Discord ID
- Access settings content without the user's authentication token
- Recover deleted data

## Technical Details

### Authentication Flow
1. User initiates Discord OAuth2 authentication
2. Discord redirects to our callback with authorization code
3. We exchange code for access token with Discord API
4. We fetch Discord user ID using access token
5. Access token is immediately discarded
6. We generate user secret: `SHA256("secret:" + user_id)`
7. User receives authentication token: `Base64(secret:user_id)`

### Settings Storage Flow
1. User uploads settings via `PUT /v1/settings` with authentication token
2. We verify token by regenerating secret from user ID
3. We validate content type and size
4. We hash user ID: `SHA256(user_id)`
5. We store: `(hashed_id, settings_blob, created_at, updated_at)`
6. User can retrieve settings via `GET /v1/settings` with same token

### Caching & Performance
- ETags based on `updated_at` timestamp
- Supports conditional requests (`If-None-Match`)
- Returns `304 Not Modified` when content unchanged
- No server-side caching layer (stateless architecture)

## Contact Information

For privacy-related questions, data access requests, or to exercise your rights, please contact the service administrator.

## Changes to This Policy

We may update this privacy policy occasionally to reflect changes in our practices or for legal reasons. The "Last Updated" date at the top indicates when changes were last made.

Significant changes will be communicated through:
- Updated "Last Updated" date
- Server announcements (if applicable)
- Direct notification (for material changes)

Your continued use of the service after changes constitutes acceptance of the updated policy.

---

*This privacy policy is effective as of the date listed above and applies to all users of the EquiCloud service.*
