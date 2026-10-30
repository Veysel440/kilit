-- The tables every scenario starts from. kilit-lab creates them before each
-- run and fills them with rows (crates/kilit-lab/src/fixture.rs); kilit-core's
-- tests read them to know what the lab's tables look like.

CREATE TABLE users (
    id   BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    name VARCHAR(100) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE orders (
    id      BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    user_id BIGINT UNSIGNED NOT NULL,
    amount  INT NOT NULL,
    email   VARCHAR(255) NULL,
    note    VARCHAR(50) NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
