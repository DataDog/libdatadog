const localPlugin = {
  rules: {
    'ticket-at-the-end': ({ subject }, when = 'always') => {
      let isValid;
      if (when === 'never') {
        const regex = /\[[A-Z]+-[0-9]+\]$/;
        isValid = !regex.test(subject);
      } else {
        const regex = /\[[A-Z]+-[0-9]+\](?!$)/;
        isValid = !regex.test(subject);
      }

      return [
        isValid,
        `A ticket (e.g. [ABC-123]), if included, must ${when === 'never' ? 'not ' : ''}be at the end of the subject`,
      ];
    },
  }
};

module.exports = {
  extends: ['@commitlint/config-conventional'],
  rules: {
    'ticket-at-the-end': [
      2,  // Error
      'always'
    ],
    'subject-case': [
      0,  // Disabled
      'never',
      ['sentence-case', 'start-case', 'pascal-case', 'upper-case'],
    ]
  },
  plugins: [localPlugin]
};
