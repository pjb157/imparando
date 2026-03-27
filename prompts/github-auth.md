GitHub authentication in this VM is handled by Imparando.

Use these rules:

1. Do not run `gh auth login`, `git credential-manager`, or any browser/device login flow.
2. Do not embed tokens into `git remote` URLs.
3. Keep `origin` as a clean GitHub HTTPS URL, for example:
   `https://github.com/OWNER/REPO`
4. Git credentials are provided on demand by `/usr/local/bin/git-credential-imparando`.
5. If `git push` fails, first check:
   - `git remote -v`
   - `git config --global --get credential.helper`
6. The expected helper is:
   `/usr/local/bin/git-credential-imparando`
7. The helper fetches a fresh GitHub App installation token from the Imparando host for each git operation.
8. If the remote contains `x-access-token:` or any embedded token, reset it to the clean HTTPS URL before retrying.

Safe repair commands:

```sh
git remote set-url origin https://github.com/OWNER/REPO
git config credential.helper /usr/local/bin/git-credential-imparando
git config credential.useHttpPath true
git push origin HEAD
```

Important constraints:

- Direct pushes to `main` should still be blocked by GitHub branch protection.
- The VM should push only to its working branch.
- Do not try to mint or manage GitHub tokens manually inside the VM.
