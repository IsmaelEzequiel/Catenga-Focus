import { Resend } from "resend";

export type Mailer = {
  sendLoginLink(to: string, name: string, link: string): Promise<void>;
};

const SUBJECT = "Sign in to Catenga Focus";

const body = (name: string, link: string): string =>
  `Hi ${name},

Sign in to Catenga Focus: ${link}

The link works once and expires in 15 minutes. If you didn't ask for it, ignore this email.
`;

export function resendMailer(apiKey: string, from: string): Mailer {
  const resend = new Resend(apiKey);
  return {
    sendLoginLink: async (to, name, link) => {
      const { error } = await resend.emails.send({ from, to, subject: SUBJECT, text: body(name, link) });
      if (error) throw new Error(`Resend: ${error.name}: ${error.message}`);
    },
  };
}

/** Local dev: prints the link instead of sending it. */
export function consoleMailer(): Mailer {
  return {
    sendLoginLink: async (to, _name, link) => {
      console.log(`login link for ${to}: ${link}`);
    },
  };
}
