# Rules

Below the list of rules supported by Postgres Language Tools, divided by group. Here's a legend of the emojis:

- The icon ✅ indicates that the rule is part of the recommended rules.

[//]: # (BEGIN RULES_INDEX)

## Safety

Rules that detect potential safety issues in your code.

| Rule name | Description | Properties |
| --- | --- | --- |
| [addingRequiredField](./adding-required-field) | Adding a new column that is NOT NULL and has no default value to an existing table effectively makes it required. |  |
| [banDropColumn](./ban-drop-column) | Dropping a column may break existing clients. | ✅ |
| [banDropDatabase](./ban-drop-database) | Dropping a database may break existing clients (and everything else, really). |  |
| [banDropNotNull](./ban-drop-not-null) | Dropping a NOT NULL constraint may break existing clients. | ✅ |
| [banDropTable](./ban-drop-table) | Dropping a table may break existing clients. | ✅ |
| [banTruncateCascade](./ban-truncate-cascade) | Using `TRUNCATE`'s `CASCADE` option will truncate any tables that are also foreign-keyed to the specified tables. |  |

[//]: # (END RULES_INDEX)


