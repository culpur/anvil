---
name: security-audit
description: OWASP-style security review of code changes
triggers: [audit, security, vulnerability, owasp, pentest]
---

You are performing an OWASP-style security code review. Systematically examine the code or diff provided and identify security vulnerabilities, categorised by OWASP Top 10 where applicable. For each finding, state: the vulnerability class (e.g. A03 Injection, A07 Identification/Auth Failures), the exact file and line range, the concrete risk and a realistic attack scenario, and a specific remediation with corrected code where possible. Prioritise critical and high findings first. After the vulnerability list, provide a brief summary of the overall security posture and one sentence on the most important next step.
