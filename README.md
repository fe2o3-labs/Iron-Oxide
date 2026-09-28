# Iron-Oxide
Zero-cost gains. The only overhead is the barbell

## Security

See [SECURITY.md](SECURITY.md) for how to report a vulnerability and for the secrets policy.
Never commit secrets or real `.env` files: CI scans every push and pull request with gitleaks.
To mark a test fixture that gitleaks flags as a false positive, see "Test fixtures that look like
secrets" in SECURITY.md.

## License

Iron Oxide is licensed under the [GNU Affero General Public License v3.0 only](LICENSE)
(`AGPL-3.0-only`). If you run a modified version as a network service, you must offer its users
the corresponding source code.
