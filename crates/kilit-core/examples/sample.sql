-- SQL that Laravel 13 generates for migrations on `orders`, plus a few
-- statements that show what kilit does with SQL it cannot read or check.

alter table `orders` add `coupon` varchar(255) null after `amount`;
alter table `orders` modify `amount` bigint not null;
alter table `orders` add index `orders_amount_index`(`amount`);
alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`) on delete cascade;
alter table `orders` add fulltext `orders_note_fulltext`(`note`);
alter table `orders` rename column `note` to `memo`;
alter table `orders` drop `legacy_code`, drop `legacy_flag`;

-- Schema::disableForeignKeyConstraints() ... enableForeignKeyConstraints()
SET FOREIGN_KEY_CHECKS=0;
alter table `orders` add constraint `orders_shop_id_foreign` foreign key (`shop_id`) references `shops` (`id`);
SET FOREIGN_KEY_CHECKS=1;

-- sqlparser cannot read this one yet
ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;

-- a semicolon inside a string does not end the statement
INSERT INTO settings (name, value) VALUES ('separator', ';');
