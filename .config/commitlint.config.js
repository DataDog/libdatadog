const localPlugin = {
  rules: {
    'ticket-at-the-end': (parsed, when = 'always') => {
      const { subject } = parsed;

      const regex = /\[[A-Z]+-[0-9]+\](?!$)/;

      const isValid = !regex.test(subject.trim());
      const negated = when === 'never';

      return [
        negated ? !isValid : isValid,
        `A ticket (e.g. [ABC-123]), if included, must ${negated ? 'not ' : ''}be at the end of the subject`,
      ];
    },
  }
}

module.exports = {
  extends: ['@commitlint/config-conventional'],
  rules: {
    'ticket-at-the-end': [2, 'always'],
  },
  plugins: [localPlugin]
}
