-- SQL that Laravel 13 generates for migrations on `orders`, checked against
-- the tables in mysql-schema.sql, plus a few statements that show what kilit
-- does with SQL it cannot read or check.

-- $table->string('coupon')->nullable()->after('amount');
alter table `orders` add `coupon` varchar(255) null after `amount`;

-- $table->bigInteger('amount')->change();
alter table `orders` modify `amount` bigint not null;

-- $table->string('note', 60)->nullable()->change();
alter table `orders` modify `note` varchar(60) null;

-- $table->string('status', 30)->change(); since Laravel 11 this also drops
-- the column's default, because the call does not repeat ->default('new')
alter table `orders` modify `status` varchar(30) not null;

-- $table->unsignedInteger('user_id')->change(); user_id has a foreign key
alter table `orders` modify `user_id` int unsigned not null;

-- $table->index('amount');
alter table `orders` add index `orders_amount_index`(`amount`);

-- $table->fullText('note');
alter table `orders` add fulltext `orders_note_fulltext`(`note`);

-- $table->string('channel')->nullable(); the table has a FULLTEXT index now
alter table `orders` add `channel` varchar(255) null;

-- $table->renameColumn('note', 'memo');
alter table `orders` rename column `note` to `memo`;

-- $table->dropColumn(['legacy_code', 'legacy_flag']);
alter table `orders` drop `legacy_code`, drop `legacy_flag`;

-- $table->foreignId('shop_id')->nullable()->constrained(), inside
-- Schema::disableForeignKeyConstraints() ... enableForeignKeyConstraints()
SET FOREIGN_KEY_CHECKS=0;
alter table `orders` add `shop_id` bigint unsigned null;
alter table `orders` add constraint `orders_shop_id_foreign` foreign key (`shop_id`) references `shops` (`id`);
SET FOREIGN_KEY_CHECKS=1;

-- sqlparser cannot read this one yet, so kilit stops trusting what it knew
-- about `orders`
ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;
alter table `orders` modify `memo` varchar(100) null;

-- a semicolon inside a string does not end the statement
INSERT INTO settings (name, value) VALUES ('separator', ';');
